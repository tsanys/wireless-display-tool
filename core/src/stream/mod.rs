//! Pipeline streaming: capture → (downscale) → encode H.264 → WebRTC track.
//!
//! Dipakai jalur produksi sender (T6). Desain:
//! - **Pacing**: satu frame in-flight. `tokio::time::sleep_until` per frame;
//!   bila pekerjaan sebelumnya melewati jadwal, frame berikutnya **di-skip**
//!   (drop) alih-alih menumpuk buffer — memory tetap stabil.
//! - **Resolusi**: frame capture di-downscale ke target (mis. 720p) dengan
//!   box filter BGRA (tanpa dependency tambahan).
//! - **Threading**: `ScreenCapturer`/`FrameEncoder` native tidak `Send`
//!   (VideoToolbox dan Media Foundation COM terikat thread), jadi pipeline
//!   harus dijalankan pada **thread dedikasi** yang membuat dan memakai
//!   keduanya (lihat `sender_session::start_pipeline`). `capture_frame`
//!   boleh sinkron/blocking di thread itu; hanya penulisan sample yang
//!   async (packetization RTP oleh library).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rtc::media::Sample;
use rtc::rtp_transceiver::{PayloadType, SSRC};
use tokio::sync::watch;

use crate::capture::{CaptureError, Frame, ScreenCapturer};
use crate::encode::{EncodeError, EncodedPacket, FrameEncoder};

/// Error pipeline.
#[derive(Debug)]
pub enum StreamError {
    Capture(String),
    Encode(String),
    Sink(String),
    Config(String),
    /// Error capture audio (jalur audio R4).
    Audio(String),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Capture(m) => write!(f, "capture: {m}"),
            StreamError::Encode(m) => write!(f, "encode: {m}"),
            StreamError::Sink(m) => write!(f, "sink: {m}"),
            StreamError::Config(m) => write!(f, "config: {m}"),
            StreamError::Audio(m) => write!(f, "audio: {m}"),
        }
    }
}

impl std::error::Error for StreamError {}

impl From<CaptureError> for StreamError {
    fn from(e: CaptureError) -> Self {
        StreamError::Capture(e.to_string())
    }
}

impl From<EncodeError> for StreamError {
    fn from(e: EncodeError) -> Self {
        StreamError::Encode(e.to_string())
    }
}

impl From<crate::audio::AudioCaptureError> for StreamError {
    fn from(e: crate::audio::AudioCaptureError) -> Self {
        StreamError::Audio(e.to_string())
    }
}

/// Konfigurasi pipeline.
#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    /// Lebar frame yang dikirim (target, setelah downscale).
    pub width: u32,
    /// Tinggi frame yang dikirim (target, setelah downscale).
    pub height: u32,
    /// Target frame per detik.
    pub fps: u32,
    /// Target bitrate encoder.
    pub bitrate_bps: u32,
    /// Interval keyframe (frame).
    pub keyframe_interval: u32,
}

impl PipelineConfig {
    /// 720p30 — target uji E2E (cocok dengan H.264 level 3.1 yang
    /// dinegosiasikan, aman untuk TV low-end).
    pub fn hd720p30() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_bps: 5_000_000,
            keyframe_interval: 60,
        }
    }

    fn frame_duration(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / self.fps.max(1) as u64)
    }
}

/// Statistik runtime pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct PipelineStats {
    /// Sample H.264 yang berhasil dikirim.
    pub frames: u64,
    /// Frame di-skip karena tertinggal jadwal (drop).
    pub skipped: u64,
    /// Iterasi gagal (capture/encode/sink).
    pub errors: u64,
    /// fps rata-rata (dihitung sejak start).
    pub fps: f32,
}

/// Tujuan penulisan sample H.264.
#[async_trait::async_trait]
pub trait SampleSink: Send + Sync {
    /// Kirim satu sample H.264 (Annex B) dengan durasi presentasi.
    async fn write(&self, data: Vec<u8>, duration: Duration) -> Result<(), StreamError>;
}

/// Sink ke `TrackLocalStaticSample` WebRTC (packetization RTP oleh library).
pub struct TrackSink {
    track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
    ssrc: SSRC,
    payload_type: PayloadType,
}

impl TrackSink {
    /// `ssrc`/`payload_type` harus yang **ternegosiasi** (lihat
    /// `SenderPeer::video_send_params`).
    pub fn new(
        track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
        ssrc: SSRC,
        payload_type: PayloadType,
    ) -> Self {
        Self {
            track,
            ssrc,
            payload_type,
        }
    }
}

#[async_trait::async_trait]
impl SampleSink for TrackSink {
    async fn write(&self, data: Vec<u8>, duration: Duration) -> Result<(), StreamError> {
        // webrtc 0.21: `Sample` tidak lagi `Default` — `timestamp` adalah
        // observasi wall-clock yang wajib diisi pemanggil.
        let sample = Sample {
            data: Bytes::from(data),
            timestamp: Instant::now(),
            duration,
            packet_timestamp: 0,
            prev_dropped_packets: 0,
            prev_padding_packets: 0,
        };
        self.track
            .sample_writer(self.ssrc, self.payload_type)
            .write_sample(&sample)
            .await
            .map_err(|e| StreamError::Sink(e.to_string()))
    }
}

/// Jalankan pipeline sampai `stop` bernilai true.
///
/// `on_stats` dipanggil tiap ~2 detik (untuk UI/log).
pub async fn run<C, E, S, F>(
    mut capturer: C,
    mut encoder: E,
    sink: S,
    cfg: PipelineConfig,
    mut stop: watch::Receiver<bool>,
    mut on_stats: F,
) -> Result<PipelineStats, StreamError>
where
    // Catatan: C/E tidak perlu `Send` — dibuat dan dipakai pada thread yang
    // sama (pipeline berjalan di thread dedikasi).
    C: ScreenCapturer,
    E: FrameEncoder,
    S: SampleSink + 'static,
    F: FnMut(PipelineStats),
{
    if cfg.fps == 0 {
        return Err(StreamError::Config("fps tidak boleh 0".into()));
    }
    let frame_duration = cfg.frame_duration();
    let started = Instant::now();
    let mut stats = PipelineStats::default();
    let mut last_stats = Instant::now();
    let mut deadline = Instant::now();

    // Nilai awal: `changed()` hanya menyala saat nilai BERUBAH, jadi stop
    // yang sudah true sebelum start harus dicek di sini.
    if *stop.borrow() {
        return Ok(stats);
    }

    loop {
        tokio::select! {
            res = stop.changed() => {
                match res {
                    // Berubah → berhenti hanya bila nilainya true.
                    Ok(()) => { if *stop.borrow() { break; } }
                    // Sender di-drop → anggap permintaan stop.
                    Err(_) => break,
                }
            }
            _ = tokio::time::sleep_until(deadline.into()) => {}
        }

        // Jadwal frame berikutnya.
        deadline += frame_duration;

        // Tertinggal jadwal → drop frame ini (jangan menumpuk buffer).
        if Instant::now() > deadline {
            stats.skipped += 1;
        } else {
            match capture_encode_send(&mut capturer, &mut encoder, &sink, &cfg, frame_duration)
                .await
            {
                Ok(n) => stats.frames += n,
                Err(e) => {
                    stats.errors += 1;
                    // Error transien (mis. frame belum siap) tidak fatal;
                    // dicatat lalu lanjut.
                    tracing::warn!("pipeline error: {e}");
                }
            }
        }

        let elapsed = started.elapsed().as_secs_f32();
        if elapsed > 0.0 {
            stats.fps = stats.frames as f32 / elapsed;
        }
        if last_stats.elapsed() >= Duration::from_secs(2) {
            last_stats = Instant::now();
            on_stats(stats);
        }
    }

    let elapsed = started.elapsed().as_secs_f32();
    if elapsed > 0.0 {
        stats.fps = stats.frames as f32 / elapsed;
    }
    Ok(stats)
}

/// Satu iterasi: capture (blocking) → downscale → encode → tulis sample.
async fn capture_encode_send<C, E, S>(
    capturer: &mut C,
    encoder: &mut E,
    sink: &S,
    cfg: &PipelineConfig,
    frame_duration: Duration,
) -> Result<u64, StreamError>
where
    C: ScreenCapturer,
    E: FrameEncoder,
    S: SampleSink,
{
    // capture_frame sinkron/blocking (ScreenCaptureKit memompa runloop;
    // Windows menunggu channel). Aman: pipeline berjalan di thread dedikasi.
    let raw = capturer.capture_frame()?;
    // Compose ke ukuran target: skala proporsional + letterbox hitam.
    // Target = ukuran panel TV (1920x1080) agar TV tampil 1:1 tanpa scaling.
    let scaled = scale_letterbox_bgra(&raw, cfg.width, cfg.height)?;
    let packets: Vec<EncodedPacket> = encoder.encode_frame(&scaled)?;

    let mut written = 0u64;
    for pkt in packets {
        if pkt.data.is_empty() {
            continue;
        }
        sink.write(pkt.data, frame_duration).await?;
        written += 1;
    }
    Ok(written)
}

/// Downscale BGRA dengan box filter (rata-rata area sumber per piksel tujuan).
///
/// Menghormati `stride` sumber; output rapat (`dst_w * 4` per baris).
pub fn downscale_bgra(src: &Frame, dst_w: u32, dst_h: u32) -> Result<Frame, StreamError> {
    if dst_w == 0 || dst_h == 0 {
        return Err(StreamError::Config("ukuran target 0".into()));
    }
    if src.width == 0 || src.height == 0 {
        return Err(StreamError::Capture("frame sumber 0x0".into()));
    }
    if src.width == dst_w && src.height == dst_h {
        return Ok(src.clone());
    }
    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];
    blit_scaled(src, &mut out, dst_w, 0, 0, dst_w, dst_h)?;
    Ok(Frame {
        width: dst_w,
        height: dst_h,
        stride: dst_w * 4,
        format: src.format,
        data: out,
    })
}

/// Compose BGRA ke canvas `dst_w x dst_h`: konten di-scale **proporsional**
/// (sekali, box filter) lalu diletakkan di tengah, sisanya hitam.
///
/// Tujuan (T6, laporan "masih buram"): stream dikirim pada ukuran panel TV
/// (1920x1080) sehingga TV menampilkan **1:1 tanpa scaling** — menghilangkan
/// softness dari resampling GL di TV untuk konten beraspek beda (16:10 laptop
/// vs 16:9 TV).
pub fn scale_letterbox_bgra(src: &Frame, dst_w: u32, dst_h: u32) -> Result<Frame, StreamError> {
    if dst_w == 0 || dst_h == 0 {
        return Err(StreamError::Config("ukuran target 0".into()));
    }
    if src.width == 0 || src.height == 0 {
        return Err(StreamError::Capture("frame sumber 0x0".into()));
    }

    // Skala proporsional (tidak pernah upscale) agar aspek terjaga.
    let scale = (dst_w as f64 / src.width as f64)
        .min(dst_h as f64 / src.height as f64)
        .min(1.0);
    let inner_w = (((src.width as f64 * scale) as u32) & !1).clamp(2, dst_w & !1);
    let inner_h = (((src.height as f64 * scale) as u32) & !1).clamp(2, dst_h & !1);
    let off_x = ((dst_w - inner_w) / 2) & !1;
    let off_y = ((dst_h - inner_h) / 2) & !1;

    // Canvas hitam opaque (BGRA).
    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    blit_scaled(src, &mut out, dst_w, off_x, off_y, inner_w, inner_h)?;

    Ok(Frame {
        width: dst_w,
        height: dst_h,
        stride: dst_w * 4,
        format: src.format,
        data: out,
    })
}

/// Skala `src` (BGRA) ke region `inner_w x inner_h` pada offset (`off_x`,`off_y`)
/// di dalam buffer tujuan `dst` (lebar `dst_w`, stride `dst_w*4`).
fn blit_scaled(
    src: &Frame,
    dst: &mut [u8],
    dst_w: u32,
    off_x: u32,
    off_y: u32,
    inner_w: u32,
    inner_h: u32,
) -> Result<(), StreamError> {
    if inner_w == 0 || inner_h == 0 {
        return Err(StreamError::Config("ukuran inner 0".into()));
    }
    let dst_need = (dst_w as usize)
        .checked_mul((off_y + inner_h) as usize)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| StreamError::Config("dst terlalu kecil".into()))?;
    if dst.len() < dst_need {
        return Err(StreamError::Config("buffer dst terpotong".into()));
    }

    let src_bpp = 4usize; // BGRA8
    let src_stride = src.stride as usize;
    let need = src_stride
        .checked_mul(src.height as usize)
        .ok_or_else(|| StreamError::Capture("stride*height overflow".into()))?;
    if src.data.len() < need {
        return Err(StreamError::Capture("buffer frame terpotong".into()));
    }

    let src_w = src.width as u64;
    let src_h = src.height as u64;

    for dy in 0..inner_h as u64 {
        let y0 = (dy * src_h / inner_h as u64) as usize;
        let y1 = (((dy + 1) * src_h / inner_h as u64) as usize).clamp(y0 + 1, src.height as usize);
        for dx in 0..inner_w as u64 {
            let x0 = (dx * src_w / inner_w as u64) as usize;
            let x1 =
                (((dx + 1) * src_w / inner_w as u64) as usize).clamp(x0 + 1, src.width as usize);

            let mut acc = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1 {
                let row = sy * src_stride;
                for sx in x0..x1 {
                    let off = row + sx * src_bpp;
                    acc[0] += src.data[off] as u32;
                    acc[1] += src.data[off + 1] as u32;
                    acc[2] += src.data[off + 2] as u32;
                    acc[3] += src.data[off + 3] as u32;
                    count += 1;
                }
            }
            let count = count.max(1);
            let px = off_x as u64 + dx;
            let py = off_y as u64 + dy;
            let dst_off = (py as usize * dst_w as usize + px as usize) * src_bpp;
            dst[dst_off] = (acc[0] / count) as u8;
            dst[dst_off + 1] = (acc[1] / count) as u8;
            dst[dst_off + 2] = (acc[2] / count) as u8;
            dst[dst_off + 3] = (acc[3] / count) as u8;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::PixelFormat;
    use crate::encode::{EncoderConfig, H264EntropyMode, H264Profile, H264Tuning};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn solid_frame(w: u32, h: u32, px: [u8; 4]) -> Frame {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for c in data.chunks_exact_mut(4) {
            c.copy_from_slice(&px);
        }
        Frame {
            width: w,
            height: h,
            stride: w * 4,
            format: PixelFormat::Bgra8,
            data,
        }
    }

    #[test]
    fn downscale_preserves_color_and_size() {
        let src = solid_frame(64, 48, [10, 20, 30, 255]);
        let out = downscale_bgra(&src, 32, 24).expect("downscale");
        assert_eq!((out.width, out.height), (32, 24));
        assert_eq!(out.stride, 32 * 4);
        assert_eq!(out.data.len(), 32 * 24 * 4);
        // Warna solid harus tetap sama setelah rata-rata.
        assert_eq!(&out.data[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn downscale_honors_source_stride() {
        // Baris sumber punya padding (stride > w*4).
        let w = 8u32;
        let h = 4u32;
        let stride = w * 4 + 16;
        let mut data = vec![0u8; (stride * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let off = y * stride as usize + x * 4;
                data[off..off + 4].copy_from_slice(&[1, 2, 3, 255]);
            }
        }
        let src = Frame {
            width: w,
            height: h,
            stride,
            format: PixelFormat::Bgra8,
            data,
        };
        let out = downscale_bgra(&src, 4, 2).expect("downscale");
        assert_eq!(&out.data[0..4], &[1, 2, 3, 255]);
    }

    #[test]
    fn letterbox_preserves_aspect_and_pads_black() {
        // Sumber 4:3 (64x48) ke target 16:9 (32x32) -> konten 32x24 di tengah,
        // bar hitam atas/bawah.
        let src = solid_frame(64, 48, [10, 200, 30, 255]);
        let out = scale_letterbox_bgra(&src, 32, 32).expect("letterbox");
        assert_eq!((out.width, out.height), (32, 32));
        assert_eq!(out.data.len(), 32 * 32 * 4);

        // Baris paling atas hitam opaque.
        assert_eq!(&out.data[0..4], &[0, 0, 0, 255]);
        // Baris tengah = warna konten.
        let mid = (16 * 32 + 16) * 4;
        assert_eq!(&out.data[mid..mid + 4], &[10, 200, 30, 255]);
    }

    #[test]
    fn letterbox_does_not_upscale_small_source() {
        // Sumber lebih kecil dari target: tetap 1:1 (tanpa upscale), sisanya hitam.
        let src = solid_frame(320, 240, [1, 2, 3, 255]);
        let out = scale_letterbox_bgra(&src, 1920, 1080).expect("letterbox");
        assert_eq!((out.width, out.height), (1920, 1080));
        // Pojok kiri-atas hitam (konten 320x240 tidak menutupi).
        assert_eq!(&out.data[0..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn downscale_rejects_bad_input() {
        let src = solid_frame(8, 8, [0, 0, 0, 255]);
        assert!(downscale_bgra(&src, 0, 8).is_err());
        let mut short = src.clone();
        short.data.truncate(8);
        assert!(downscale_bgra(&short, 4, 4).is_err());
    }

    /// Capturer palsu: frame solid, hitung berapa kali dipanggil.
    struct FakeCapturer {
        calls: Arc<AtomicU64>,
    }

    impl ScreenCapturer for FakeCapturer {
        fn new() -> Result<Self, CaptureError> {
            unreachable!("pakai FakeCapturer::with_counter")
        }
        fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(solid_frame(64, 48, [200, 100, 50, 255]))
        }
        fn display_size(&self) -> (u32, u32) {
            (64, 48)
        }
    }

    /// Encoder palsu: satu paket Annex B kecil per frame.
    struct FakeEncoder;

    impl FrameEncoder for FakeEncoder {
        fn new(_config: EncoderConfig) -> Result<Self, EncodeError> {
            Ok(FakeEncoder)
        }
        fn encode_frame(&mut self, _frame: &Frame) -> Result<Vec<EncodedPacket>, EncodeError> {
            Ok(vec![EncodedPacket {
                data: vec![0, 0, 0, 1, 0x65, 0xAA],
                is_keyframe: false,
                pts: 0,
            }])
        }
        fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
            Ok(vec![])
        }
    }

    /// Sink palsu: hitung sample + byte.
    #[derive(Default)]
    struct FakeSink {
        samples: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl SampleSink for FakeSink {
        async fn write(&self, data: Vec<u8>, _duration: Duration) -> Result<(), StreamError> {
            assert!(!data.is_empty());
            self.samples.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    /// Pipeline mengirim sample dan berhenti bersih saat stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pipeline_streams_until_stopped() {
        let calls = Arc::new(AtomicU64::new(0));
        let sink = FakeSink::default();
        let samples = sink.samples.clone();
        let (stop_tx, stop_rx) = watch::channel(false);

        let cfg = PipelineConfig {
            width: 32,
            height: 24,
            fps: 60,
            bitrate_bps: 1_000_000,
            keyframe_interval: 30,
        };

        let stats_tx = Arc::new(std::sync::Mutex::new(Vec::<PipelineStats>::new()));
        let sink_stats = stats_tx.clone();
        let handle = tokio::spawn(run(
            FakeCapturer {
                calls: calls.clone(),
            },
            FakeEncoder,
            sink,
            cfg,
            stop_rx,
            move |s| sink_stats.lock().unwrap().push(s),
        ));

        tokio::time::sleep(Duration::from_millis(250)).await;
        stop_tx.send(true).unwrap();
        let stats = handle.await.expect("join").expect("pipeline ok");

        assert!(stats.frames > 0, "harus ada sample terkirim");
        assert_eq!(stats.frames, samples.load(Ordering::Relaxed));
        assert!(calls.load(Ordering::Relaxed) > 0, "capture harus dipanggil");
    }

    /// Stop yang sudah true sebelum start → keluar tanpa mengirim.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pipeline_stops_immediately_when_already_stopped() {
        let (_tx, rx) = watch::channel(true);
        let stats = run(
            FakeCapturer {
                calls: Arc::new(AtomicU64::new(0)),
            },
            FakeEncoder,
            FakeSink::default(),
            PipelineConfig::hd720p30(),
            rx,
            |_| {},
        )
        .await
        .expect("pipeline ok");
        assert_eq!(stats.frames, 0);
    }

    #[test]
    fn hd720p30_config_is_consistent() {
        let c = PipelineConfig::hd720p30();
        assert_eq!((c.width, c.height, c.fps), (1280, 720, 30));
        assert_eq!(c.frame_duration(), Duration::from_nanos(33_333_333));
        // Default produksi tetap Baseline+CAVLC sampai A/B device selesai.
        assert_eq!(
            H264Tuning::default(),
            H264Tuning {
                profile: H264Profile::Baseline,
                entropy_mode: H264EntropyMode::Cavlc,
            }
        );
    }
}
