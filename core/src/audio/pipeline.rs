//! Pump pipeline audio: capture → normalisasi → 48 kHz → chunk 20 ms →
//! Opus → WebRTC audio track.
//!
//! Berjalan di **thread dedikasi** (capturer native tidak `Send`), sama
//! seperti pipeline video. Perbedaan penting dengan video:
//! - **Blocking-read driven**, bukan deadline-driven: kita menulis paket
//!   tepat sebanyak sampel yang diterima, sehingga timestamp RTP (dihitung
//!   webrtc-rs dari akumulasi duration) tidak drift.
//! - **Tidak ada antrean tak terbatas**: tiap iterasi memproses frame yang
//!   dibaca lalu selesai. Bila capturer tertinggal, platform membuang data
//!   basi di sisi sumber (bounded queue + drop-oldest) — bukan menumpuk.
//! - **Route watch**: `active=false` (Laptop/Muted) → capturer dilepas dan
//!   thread menunggu; `active=true` (TV/Both) → capturer dibuat ulang.
//!   Route switch tidak pernah menyentuh transceiver/video.
//! - **Error audio tidak fatal**: capturer gagal → rebuild dengan backoff,
//!   video tetap berjalan.

use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::opus_encode::OpusStreamEncoder;
use super::pcm::OpusFrameChunker;
use super::resample::StreamResampler;
use super::{AudioCaptureError, AudioFrame, OUT_CHUNK_DURATION, SystemAudioCapturer};
use crate::stream::{SampleSink, StreamError};

/// Konfigurasi pipeline audio.
#[derive(Debug, Clone, Copy)]
pub struct AudioPipelineConfig {
    /// Bitrate Opus (bit/s).
    pub bitrate_bps: i32,
}

impl Default for AudioPipelineConfig {
    fn default() -> Self {
        Self {
            bitrate_bps: super::opus_encode::DEFAULT_BITRATE_BPS,
        }
    }
}

/// Statistik runtime audio (dipublikasikan ~tiap 2 dtk).
#[derive(Debug, Default, Clone)]
pub struct AudioPipelineStats {
    /// Chunk PCM yang dibaca dari capturer.
    pub capture_frames: u64,
    /// Paket Opus yang ditulis ke track.
    pub packets_sent: u64,
    /// Chunk dibuang (sink gagal / chunk tidak lengkap saat stop).
    pub dropped: u64,
    /// Error capture (Termasuk yang memicu reconnect).
    pub errors: u64,
    /// Capturer dibangun ulang (device berubah / recovery).
    pub reconnects: u64,
    /// Durasi media yang terkirim (packet × 20 ms).
    pub sent_ms: u64,
    /// RMS chunk terakhir (0.0–1.0) untuk indikator level.
    pub level: f32,
    /// Pesan error terakhir (untuk UI), None bila sehat.
    pub last_error: Option<String>,
}

impl AudioPipelineStats {
    fn publish(&mut self, last_stats: &mut Instant, on_stats: &mut dyn FnMut(&AudioPipelineStats)) {
        if last_stats.elapsed() >= Duration::from_secs(2) {
            *last_stats = Instant::now();
            on_stats(self);
        }
    }
}

/// Backoff rebuild capturer.
fn backoff_delay(attempt: u32) -> Duration {
    Duration::from_millis(match attempt {
        0 => 250,
        1 => 500,
        2 => 1000,
        _ => 2000,
    })
}

/// Jalankan pump audio sampai `stop` true.
///
/// `make_capturer` dipanggil **di thread ini** setiap kali route aktif atau
/// setelah error; capturer dibuat/dipakai/dibuang di thread yang sama
/// (objek SCK/COM tidak `Send`). `on_stats` dipanggil ~tiap 2 dtk.
pub async fn run<C, F, S, G>(
    mut make_capturer: F,
    sink: S,
    cfg: AudioPipelineConfig,
    mut active: watch::Receiver<bool>,
    mut stop: watch::Receiver<bool>,
    mut on_stats: G,
) -> Result<AudioPipelineStats, StreamError>
where
    C: SystemAudioCapturer,
    F: FnMut() -> Result<C, AudioCaptureError>,
    S: SampleSink + 'static,
    G: FnMut(&AudioPipelineStats),
{
    let mut stats = AudioPipelineStats::default();
    let mut last_stats = Instant::now();

    let mut encoder = OpusStreamEncoder::with_bitrate(cfg.bitrate_bps)
        .map_err(|e| StreamError::Config(format!("opus: {e}")))?;
    let mut chunker = OpusFrameChunker::new();
    let mut resampler: Option<StreamResampler> = None;
    let mut capturer: Option<C> = None;
    let mut fail_attempt: u32 = 0;
    // Frame reusable: capturer pull-based (WASAPI) memakai ulang
    // `frame.samples` sehingga tidak ada alokasi per-paket.
    let mut frame = AudioFrame::empty();

    if *stop.borrow() {
        return Ok(stats);
    }

    loop {
        if *stop.borrow() {
            break;
        }

        // --- Route tidak aktif: lepas capturer, tunggu perubahan. ---
        if !*active.borrow() {
            capturer = None;
            resampler = None;
            chunker = OpusFrameChunker::new();
            fail_attempt = 0;
            tokio::select! {
                r = stop.changed() => { if r.is_err() { break; } }
                r = active.changed() => { if r.is_err() { break; } }
            }
            continue;
        }

        // --- Buat capturer bila perlu. ---
        if capturer.is_none() {
            match make_capturer() {
                Ok(c) => {
                    let rate = c.sample_rate();
                    let channels = c.channels();
                    resampler = Some(StreamResampler::new(rate, channels)?);
                    chunker = OpusFrameChunker::new();
                    capturer = Some(c);
                    fail_attempt = 0;
                    stats.last_error = None;
                }
                Err(e) => {
                    stats.errors += 1;
                    stats.reconnects += 1;
                    stats.last_error = Some(e.to_string());
                    tracing::warn!("audio capturer gagal: {e}");
                    let delay = backoff_delay(fail_attempt);
                    fail_attempt = fail_attempt.saturating_add(1);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        r = stop.changed() => { if r.is_err() { break; } }
                    }
                    stats.publish(&mut last_stats, &mut on_stats);
                    continue;
                }
            }
        }

        // --- Baca satu chunk. ---
        let frame_res = capturer
            .as_mut()
            .expect("capturer ada")
            .read_frame_into(&mut frame);
        match frame_res {
            Ok(()) => {}
            Err(AudioCaptureError::Timeout(_)) => {
                // Tidak ada data dalam batas waktu — normal saat senyap;
                // cek watch lalu lanjut.
                continue;
            }
            Err(e) => {
                stats.errors += 1;
                stats.reconnects += 1;
                stats.last_error = Some(e.to_string());
                tracing::warn!("audio capture terputus: {e}");
                capturer = None;
                resampler = None;
                let delay = backoff_delay(fail_attempt);
                fail_attempt = fail_attempt.saturating_add(1);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    r = stop.changed() => { if r.is_err() { break; } }
                }
                stats.publish(&mut last_stats, &mut on_stats);
                continue;
            }
        };
        fail_attempt = 0;

        if let Err(e) = frame.validate() {
            stats.errors += 1;
            stats.last_error = Some(e.to_string());
            continue;
        }
        stats.capture_frames += 1;

        // Device berubah rate → bangun ulang resampler.
        if resampler
            .as_ref()
            .map(|r| r.input_rate() != frame.sample_rate)
            .unwrap_or(true)
        {
            resampler = Some(StreamResampler::new(frame.sample_rate, frame.channels)?);
        }

        // Frame sudah f32 interleaved stereo (kontrak capturer); pinjam
        // tanpa memindahkan buffer agar kapasitas `frame` bisa dipakai ulang.
        let resampled = resampler
            .as_mut()
            .expect("resampler ada")
            .push(&frame.samples)?;
        let chunks = chunker.push(&resampled);

        for chunk in chunks {
            // RMS untuk indikator level (i16 → [-1,1]).
            let sum_sq: f64 = chunk
                .iter()
                .map(|s| {
                    let v = *s as f64 / 32768.0;
                    v * v
                })
                .sum();
            stats.level = (sum_sq / chunk.len().max(1) as f64).sqrt() as f32;

            let packet = match encoder.encode_chunk(&chunk) {
                Ok(p) => p,
                Err(e) => {
                    stats.dropped += 1;
                    stats.errors += 1;
                    stats.last_error = Some(e.to_string());
                    continue;
                }
            };
            match sink.write(packet, OUT_CHUNK_DURATION).await {
                Ok(()) => {
                    stats.packets_sent += 1;
                    stats.sent_ms += OUT_CHUNK_DURATION.as_millis() as u64;
                }
                Err(e) => {
                    stats.dropped += 1;
                    stats.last_error = Some(e.to_string());
                    tracing::warn!("audio sink gagal: {e}");
                }
            }
        }

        stats.publish(&mut last_stats, &mut on_stats);

        // Beri kesempatan task lain di runtime yang sama (watch/stop) untuk
        // diproses. Di produksi `read_frame` sudah blocking, tetapi capturer
        // sintetis/test bisa mengembalikan segera sehingga tanpa ini runtime
        // satu-thread bisa kelaparan.
        tokio::task::yield_now().await;
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioFrame;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    struct FakeCapturer {
        reads: Arc<AtomicU64>,
        /// Jumlah frame per read (20 ms stereo @48k = 960).
        frames_per_read: usize,
        /// Gagal pada read ke-`fail_at` (0 = tidak pernah).
        fail_at: u64,
        /// Chunk rate — dibuat konstan 48k (passthrough).
        sample_rate: u32,
    }

    impl SystemAudioCapturer for FakeCapturer {
        fn new() -> Result<Self, AudioCaptureError> {
            unreachable!()
        }
        fn read_frame(&mut self) -> Result<AudioFrame, AudioCaptureError> {
            let n = self.reads.fetch_add(1, Ordering::Relaxed);
            if self.fail_at != 0 && n == self.fail_at {
                return Err(AudioCaptureError::Session("device hilang".into()));
            }
            let frames = self.frames_per_read;
            Ok(AudioFrame {
                samples: vec![0.05f32; frames * 2],
                sample_rate: self.sample_rate,
                channels: 2,
                timestamp: Duration::from_millis(n * 20),
            })
        }
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn channels(&self) -> u16 {
            2
        }
    }

    #[derive(Default)]
    struct FakeSink {
        writes: Arc<AtomicU64>,
        durations: Arc<Mutex<Vec<Duration>>>,
    }

    #[async_trait::async_trait]
    impl SampleSink for FakeSink {
        async fn write(&self, data: Vec<u8>, duration: Duration) -> Result<(), StreamError> {
            assert!(!data.is_empty());
            self.writes.fetch_add(1, Ordering::Relaxed);
            self.durations.lock().unwrap().push(duration);
            Ok(())
        }
    }

    fn fake_factory(
        reads: Arc<AtomicU64>,
        fail_at: u64,
    ) -> impl FnMut() -> Result<FakeCapturer, AudioCaptureError> {
        move || {
            Ok(FakeCapturer {
                reads: reads.clone(),
                frames_per_read: 960,
                fail_at,
                sample_rate: 48_000,
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inactive_route_sends_nothing() {
        let reads = Arc::new(AtomicU64::new(0));
        let sink = FakeSink::default();
        let writes = sink.writes.clone();
        let (_active_tx, active_rx) = watch::channel(false);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(run(
            fake_factory(reads.clone(), 0),
            sink,
            AudioPipelineConfig::default(),
            active_rx,
            stop_rx,
            |_| {},
        ));
        tokio::time::sleep(Duration::from_millis(150)).await;
        stop_tx.send(true).unwrap();
        let stats = handle.await.unwrap().unwrap();

        assert_eq!(
            writes.load(Ordering::Relaxed),
            0,
            "tidak boleh kirim sample"
        );
        assert_eq!(stats.packets_sent, 0);
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "capturer tidak boleh dibuat"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_route_sends_20ms_packets() {
        let reads = Arc::new(AtomicU64::new(0));
        let sink = FakeSink::default();
        let writes = sink.writes.clone();
        let durations = sink.durations.clone();
        let (_active_tx, active_rx) = watch::channel(true);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(run(
            fake_factory(reads.clone(), 0),
            sink,
            AudioPipelineConfig::default(),
            active_rx,
            stop_rx,
            |_| {},
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        stop_tx.send(true).unwrap();
        let stats = handle.await.unwrap().unwrap();

        assert!(
            writes.load(Ordering::Relaxed) > 0,
            "harus ada sample terkirim"
        );
        assert_eq!(stats.packets_sent, writes.load(Ordering::Relaxed));
        // Setiap sample 20 ms = identitas timestamp monotonik.
        assert!(
            durations
                .lock()
                .unwrap()
                .iter()
                .all(|d| *d == OUT_CHUNK_DURATION),
            "semua sample harus 20 ms"
        );
        // Durasi terkirim = packet × 20 ms (tanpa drift).
        assert_eq!(stats.sent_ms, stats.packets_sent * 20);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn route_switch_stops_and_resumes_without_error() {
        let reads = Arc::new(AtomicU64::new(0));
        let sink = FakeSink::default();
        let writes = sink.writes.clone();
        let (active_tx, active_rx) = watch::channel(true);
        let (stop_tx, stop_rx) = watch::channel(false);

        let handle = tokio::spawn(run(
            fake_factory(reads.clone(), 0),
            sink,
            AudioPipelineConfig::default(),
            active_rx,
            stop_rx,
            |_| {},
        ));

        tokio::time::sleep(Duration::from_millis(120)).await;
        active_tx.send(false).unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let after_inactive = writes.load(Ordering::Relaxed);
        // Selama nonaktif tidak ada penambahan sample.
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(
            writes.load(Ordering::Relaxed),
            after_inactive,
            "route nonaktif harus berhenti menulis"
        );
        // Aktifkan lagi.
        active_tx.send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            writes.load(Ordering::Relaxed) > after_inactive,
            "route aktif harus menulis lagi"
        );
        stop_tx.send(true).unwrap();
        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.errors, 0, "route switch bukan error");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capture_error_recovers_without_killing_pipeline() {
        let reads = Arc::new(AtomicU64::new(0));
        let sink = FakeSink::default();
        let writes = sink.writes.clone();
        let (_active_tx, active_rx) = watch::channel(true);
        let (stop_tx, stop_rx) = watch::channel(false);

        // Gagal pada read pertama → capturer di-rebuild, lalu sukses.
        let handle = tokio::spawn(run(
            fake_factory(reads.clone(), 1),
            sink,
            AudioPipelineConfig::default(),
            active_rx,
            stop_rx,
            |_| {},
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        stop_tx.send(true).unwrap();
        let stats = handle.await.unwrap().unwrap();

        assert!(stats.errors >= 1, "harus mencatat error capture");
        assert!(stats.reconnects >= 1, "harus rebuild capturer");
        assert!(
            writes.load(Ordering::Relaxed) > 0,
            "harus pulih dan menulis"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn already_stopped_returns_immediately() {
        let (_active_tx, active_rx) = watch::channel(true);
        let (_stop_tx, stop_rx) = watch::channel(true);
        let stats = run(
            fake_factory(Arc::new(AtomicU64::new(0)), 0),
            FakeSink::default(),
            AudioPipelineConfig::default(),
            active_rx,
            stop_rx,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(stats.packets_sent, 0);
    }
}
