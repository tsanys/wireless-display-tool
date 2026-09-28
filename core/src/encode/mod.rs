//! Hardware H.264 encoding per-platform, diunifikasi lewat trait [`FrameEncoder`].
//!
//! - Windows: Media Foundation H.264 MFT (hardware, vendor-agnostik)
//! - macOS: VideoToolbox (`VTCompressionSession`)
//!
//! Kontrak lintas platform (diputuskan sebelum implementasi):
//! - Input: [`Frame`](crate::capture::Frame) BGRA8 dari T1 (nol konversi
//!   di macOS; di Windows dicoba RGB32 dulu, fallback NV12).
//! - Output: paket Annex B; SPS/PPS disisipkan sebelum tiap IDR agar
//!   bitstream bisa di-decode standalone (ffmpeg) dan siap di-packetize
//!   ke RTP di T3.
//! - Default: 1080p30, 5 Mbps CBR, profile Main tanpa B-frame, IDR tiap
//!   60 frame. Tanpa HW encoder → error jelas, tanpa fallback software.

use crate::capture::Frame;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
pub mod macos;

/// Profile H.264 yang didukung encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum H264Profile {
    /// Main, tanpa B-frame (low-latency). Default T2.
    Main,
    /// High profile (8-bit 4:2:0), untuk A/B kualitas/efisiensi.
    High,
    /// Baseline (fallback kompatibilitas absolut).
    Baseline,
}

/// Mode entropy coding H.264.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264EntropyMode {
    /// Context-adaptive variable-length coding; kompatibel dengan Baseline.
    Cavlc,
    /// Context-adaptive binary arithmetic coding; hanya Main/High.
    Cabac,
}

/// Pasangan profile + entropy mode untuk pengujian A/B lintas SDP/encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264Tuning {
    pub profile: H264Profile,
    pub entropy_mode: H264EntropyMode,
}

impl H264Tuning {
    /// Parse nilai hook `WDT_H264_PROFILE` dan `WDT_H264_CABAC`.
    pub fn parse(profile_raw: Option<&str>, cabac_raw: Option<&str>) -> Result<Self, String> {
        let profile = match profile_raw.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("baseline") => H264Profile::Baseline,
            Some("main") => H264Profile::Main,
            Some("high") => H264Profile::High,
            Some(other) => {
                return Err(format!(
                    "WDT_H264_PROFILE tidak valid: {other:?} (pakai baseline|main|high)"
                ));
            }
        };
        let cabac = match cabac_raw.map(str::trim).filter(|v| !v.is_empty()) {
            None => profile != H264Profile::Baseline,
            Some("1" | "true" | "yes") => true,
            Some("0" | "false" | "no") => false,
            Some(other) => {
                return Err(format!("WDT_H264_CABAC tidak valid: {other:?} (pakai 0|1)"));
            }
        };
        if profile == H264Profile::Baseline && cabac {
            return Err("WDT_H264_CABAC=1 tidak kompatibel dengan profile Baseline".to_string());
        }
        Ok(Self {
            profile,
            entropy_mode: if cabac {
                H264EntropyMode::Cabac
            } else {
                H264EntropyMode::Cavlc
            },
        })
    }
}

impl Default for H264Tuning {
    fn default() -> Self {
        Self {
            profile: H264Profile::Baseline,
            entropy_mode: H264EntropyMode::Cavlc,
        }
    }
}

/// Parameter session encoder.
#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    /// Lebar frame dalam piksel.
    pub width: u32,
    /// Tinggi frame dalam piksel.
    pub height: u32,
    /// Bitrate dalam bit per detik (default 5_000_000).
    ///
    /// Menjadi target bitrate saat `quality` = `None`, dan batas bitrate
    /// saat mode quality aktif. Batas ini hard cap di VideoToolbox dan
    /// best-effort di Media Foundation (tergantung encoder vendor).
    pub bitrate_bps: u32,
    /// Framerate untuk timestamp PTS (default 30).
    pub fps: u32,
    /// Interval keyframe IDR dalam jumlah frame (default 60 = 2 dtk @30fps).
    pub keyframe_interval: u32,
    /// Profile H.264 (default Main).
    pub profile: H264Profile,
    /// Mode entropy eksplisit. `None` membiarkan encoder memilih default.
    /// CABAC tidak valid untuk Baseline.
    pub entropy_mode: Option<H264EntropyMode>,
    /// Mode **constant quality** (0.0–1.0). Bila `Some`, encoder memakai
    /// rate-control kualitas (teks/desktop jauh lebih tajam, bandwidth
    /// variabel) dengan `bitrate_bps` sebagai batas. `None` = perilaku
    /// target bitrate seperti sebelumnya.
    pub quality: Option<f32>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            bitrate_bps: 5_000_000,
            fps: 30,
            keyframe_interval: 60,
            profile: H264Profile::Main,
            entropy_mode: None,
            quality: None,
        }
    }
}

/// Satu unit output encoder: NALU Annex B (start code `0x000001`).
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    /// Bytes Annex B; bisa berisi beberapa NALU (mis. SPS+PPS+IDR).
    pub data: Vec<u8>,
    /// True jika paket memuat IDR (random access point).
    pub is_keyframe: bool,
    /// Presentation timestamp dalam satuan 1/`fps` detik.
    pub pts: u64,
}

/// Error terunifikasi untuk semua backend encoder.
#[derive(Debug)]
#[non_exhaustive]
pub enum EncodeError {
    /// Tidak ada hardware encoder yang tersedia di mesin ini.
    NoHardwareEncoder(String),
    /// Gagal setup/teardown session encoder.
    Session(String),
    /// Gagal meng-encode frame.
    Encode(String),
    /// Konfigurasi tidak didukung backend.
    Unsupported(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::NoHardwareEncoder(msg) => {
                write!(f, "tidak ada hardware encoder: {msg}")
            }
            EncodeError::Session(msg) => write!(f, "session encoder gagal: {msg}"),
            EncodeError::Encode(msg) => write!(f, "encode gagal: {msg}"),
            EncodeError::Unsupported(msg) => write!(f, "tidak didukung: {msg}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Interface generik hardware H.264 encoder.
///
/// Implementasi per-platform hidup di modul `windows` / `macos`;
/// pemanggil (T3/T4) hanya berinteraksi lewat trait ini plus
/// [`default_encoder`], tanpa `#[cfg]` apapun.
pub trait FrameEncoder: Sized {
    /// Inisialisasi session encoder hardware dengan parameter yang diberikan.
    ///
    /// Mengembalikan error [`EncodeError::NoHardwareEncoder`] yang jelas
    /// bila mesin tidak punya HW encoder (tanpa fallback software).
    fn new(config: EncoderConfig) -> Result<Self, EncodeError>;

    /// Encode 1 frame BGRA8 → paket-paket Annex B (bisa kosong bila
    /// encoder menahan frame untuk batching internal).
    fn encode_frame(&mut self, frame: &Frame) -> Result<Vec<EncodedPacket>, EncodeError>;

    /// Kuras frame yang masih tertahan di encoder (drain).
    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError>;
}

#[cfg(target_os = "windows")]
pub fn default_encoder(config: EncoderConfig) -> Result<windows::WindowsEncoder, EncodeError> {
    windows::WindowsEncoder::new(config)
}

#[cfg(target_os = "macos")]
pub fn default_encoder(config: EncoderConfig) -> Result<macos::MacosEncoder, EncodeError> {
    macos::MacosEncoder::new(config)
}

/// Batas encoder untuk gating UI (R7): resolusi maksimum yang didukung.
///
/// Nilai konservatif berbasis level H.264 yang kita negosiasikan di SDP
/// (level 4.2 = 42e02a, aman untuk 1920×1080@60 pada kedua platform target).
/// Encoder hardware (VideoToolbox/Media Foundation) praktis selalu menerima
/// 1080p; menaikkan nilai ini menuntut bump level SDP.
pub const ENCODER_MAX_WIDTH: u32 = 1920;
pub const ENCODER_MAX_HEIGHT: u32 = 1080;
/// fps maksimum yang diiklankan di SDP (`a=framerate`/expected frame rate).
pub const ENCODER_MAX_FPS: u32 = 60;

#[cfg(test)]
mod tests {
    use super::*;

    /// Bukti kompilasi bahwa pemanggil generik cukup tahu trait
    /// [`FrameEncoder`] — tanpa `#[cfg]` platform apapun.
    #[test]
    fn default_encoder_is_usable_generically() {
        fn use_generically<E: FrameEncoder>(make: fn(EncoderConfig) -> Result<E, EncodeError>) {
            let _ = make;
        }

        use_generically(default_encoder);
    }

    #[test]
    fn h264_ab_tuning_defaults_and_overrides_are_valid() {
        assert_eq!(H264Tuning::parse(None, None), Ok(H264Tuning::default()));
        assert_eq!(
            H264Tuning::parse(Some("main"), None),
            Ok(H264Tuning {
                profile: H264Profile::Main,
                entropy_mode: H264EntropyMode::Cabac,
            })
        );
        assert_eq!(
            H264Tuning::parse(Some("high"), Some("0")),
            Ok(H264Tuning {
                profile: H264Profile::High,
                entropy_mode: H264EntropyMode::Cavlc,
            })
        );
        assert!(H264Tuning::parse(Some("baseline"), Some("1")).is_err());
        assert!(H264Tuning::parse(Some("unknown"), None).is_err());
    }
}
