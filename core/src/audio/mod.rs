//! System audio capture + normalisasi + encoding Opus, diunifikasi lewat
//! trait [`SystemAudioCapturer`].
//!
//! Pipeline audio R4 (semua batas modul jelas):
//!
//! ```text
//! SystemAudioCapturer (macOS: SCK audio / Windows: WASAPI loopback)
//!     ↓ AudioFrame (i16 interleaved, rate/channels bebas)
//! pcm::convert → f32 interleaved stereo
//!     ↓
//! resample::StreamResampler → 48 kHz stereo (passthrough bila sudah 48k)
//!     ↓
//! pcm::OpusFrameChunker → chunk tetap 960 frame (20 ms)
//!     ↓
//! opus_encode::OpusStreamEncoder → paket Opus
//!     ↓
//! stream::TrackSink (audio track WebRTC, duration 20 ms per sample)
//! ```
//!
//! Timestamp RTP dihitung webrtc-rs dari **akumulasi duration** sample
//! (`TrackLocalStaticSample::write_sample`), sehingga durasi 20 ms yang
//! eksak menghasilkan timestamp monotonik tanpa drift — clock pengirim
//! tidak dipakai untuk timestamp media.
//!
//! Threading: objek capture native (SCK/COM) tidak `Send`; capturer dibuat
//! dan dipakai pada thread dedikat pipeline audio (pola sama dengan video,
//! lihat `signaling::sender_session`).

use std::time::Duration;

pub mod opus_encode;
pub mod pcm;
pub mod resample;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub mod pipeline;

/// Rate/kanal output yang dinegosiasikan di SDP dan dipakai Opus.
pub const OUT_SAMPLE_RATE: u32 = 48_000;
/// Stereo.
pub const OUT_CHANNELS: u16 = 2;
/// 20 ms @ 48 kHz stereo interleaved = 960 frame × 2 kanal.
pub const OUT_SAMPLES_PER_CHUNK: usize = 960;
/// Durasi satu paket Opus.
pub const OUT_CHUNK_DURATION: Duration = Duration::from_millis(20);

/// Satu hasil baca capturer: PCM **f32 interleaved stereo** ternormalisasi.
///
/// Capturer bertanggung jawab menormalkan format platform (mis. f32 planar
/// de-interleaved dari ScreenCaptureKit, atau mix-format WASAPI) ke bentuk
/// kanonik ini; resampler & chunker downstream menerima kontrak yang sama
/// untuk semua platform.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Sampel f32 interleaved stereo (L,R,L,R,…), rentang [-1.0, 1.0].
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// Selalu 2 setelah normalisasi capturer.
    pub channels: u16,
    /// Posisi media monotonik sejak capturer start (dari PTS platform);
    /// dipakai untuk drift-check — timestamp RTP tetap dari durasi sample.
    pub timestamp: Duration,
}

impl AudioFrame {
    /// Frame kosong (untuk reuse buffer lintas panggilan `read_frame_into`).
    pub fn empty() -> Self {
        Self {
            samples: Vec::new(),
            sample_rate: 0,
            channels: 2,
            timestamp: Duration::ZERO,
        }
    }

    /// Jumlah frame (sampel per kanal).
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Validasi konsistensi dasar (stereo interleaved).
    pub fn validate(&self) -> Result<(), AudioCaptureError> {
        if self.sample_rate == 0 {
            return Err(AudioCaptureError::Format("sample rate 0".into()));
        }
        if self.channels != 2 {
            return Err(AudioCaptureError::Format(format!(
                "capturer harus menormalkan ke stereo, dapat {} kanal",
                self.channels
            )));
        }
        if self.samples.len() % 2 != 0 {
            return Err(AudioCaptureError::Format(format!(
                "panjang buffer {} bukan kelipatan stereo (2)",
                self.samples.len()
            )));
        }
        Ok(())
    }
}

/// Error terunifikasi backend capture audio.
#[derive(Debug)]
#[non_exhaustive]
pub enum AudioCaptureError {
    /// Izin (macOS: Screen Recording — SCK audio memakai TCC yang sama).
    PermissionDenied(String),
    /// Tidak ada device audio / output.
    NoDevice(String),
    /// Gagal setup/teardown session.
    Session(String),
    /// Data/Format PCM tidak valid.
    Format(String),
    /// Tidak ada sampel dalam batas waktu (device berubah/sleep).
    Timeout(String),
}

impl std::fmt::Display for AudioCaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioCaptureError::PermissionDenied(m) => {
                write!(f, "izin capture audio ditolak: {m}")
            }
            AudioCaptureError::NoDevice(m) => write!(f, "device audio tidak tersedia: {m}"),
            AudioCaptureError::Session(m) => write!(f, "session audio gagal: {m}"),
            AudioCaptureError::Format(m) => write!(f, "format audio tidak valid: {m}"),
            AudioCaptureError::Timeout(m) => write!(f, "timeout baca audio: {m}"),
        }
    }
}

impl std::error::Error for AudioCaptureError {}

/// Interface generik capture audio sistem (loopback/render output).
///
/// Blocking-read driven: `read_frame` mengembalikan begitu chunk PCM dari
/// device tersedia (ukuran chunk bebas; normalisasi ada di downstream).
pub trait SystemAudioCapturer: Sized {
    /// Inisialisasi session capture audio sistem.
    fn new() -> Result<Self, AudioCaptureError>;

    /// Baca chunk PCM berikutnya (blocking dengan timeout internal).
    fn read_frame(&mut self) -> Result<AudioFrame, AudioCaptureError>;

    /// Baca chunk berikutnya ke `out` (buffer dipakai ulang).
    ///
    /// Implementasi default mengalokasikan frame baru; capturer pull-based
    /// (WASAPI) meng-override untuk memakai ulang kapasitas `out.samples`
    /// sehingga tidak ada alokasi per-paket.
    fn read_frame_into(&mut self, out: &mut AudioFrame) -> Result<(), AudioCaptureError> {
        *out = self.read_frame()?;
        Ok(())
    }

    /// Sample rate aktual capturer.
    fn sample_rate(&self) -> u32;

    /// Jumlah kanal aktual capturer.
    fn channels(&self) -> u16;
}

/// Hasil probe capability audio (dipakai UI sender sebagai source of truth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCapability {
    pub available: bool,
    /// Alasan user-facing bila tidak tersedia.
    pub reason: Option<String>,
}

/// Probe capability capture audio sistem pada platform berjalan.
///
/// - macOS 13+ (ScreenCaptureKit audio) + izin Screen Recording.
/// - Windows 10+ (WASAPI loopback tersedia sejak Vista).
#[cfg(target_os = "macos")]
pub fn capability() -> AudioCapability {
    macos::capability()
}

#[cfg(target_os = "windows")]
pub fn capability() -> AudioCapability {
    windows::capability()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn capability() -> AudioCapability {
    AudioCapability {
        available: false,
        reason: Some("Capture audio sistem tidak didukung di platform ini".to_string()),
    }
}

/// Bangun capturer audio untuk platform berjalan. Dipanggil di thread
/// pipeline audio (objek native tidak `Send`).
#[cfg(target_os = "macos")]
pub fn default_capturer() -> Result<macos::MacosAudioCapturer, AudioCaptureError> {
    macos::MacosAudioCapturer::new()
}

#[cfg(target_os = "windows")]
pub fn default_capturer() -> Result<windows::WasapiLoopbackCapturer, AudioCaptureError> {
    windows::WasapiLoopbackCapturer::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_frame_validates_shape() {
        let f = AudioFrame {
            samples: vec![0.1, 0.2, 0.3, 0.4],
            sample_rate: 48_000,
            channels: 2,
            timestamp: Duration::ZERO,
        };
        assert_eq!(f.frames(), 2);
        assert!(f.validate().is_ok());

        let bad = AudioFrame {
            samples: vec![0.1, 0.2, 0.3],
            sample_rate: 48_000,
            channels: 2,
            timestamp: Duration::ZERO,
        };
        assert!(bad.validate().is_err());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn default_capturer_is_usable_generically() {
        fn use_generically<C: SystemAudioCapturer>(make: fn() -> Result<C, AudioCaptureError>) {
            let _ = make;
        }
        use_generically(default_capturer);
    }
}
