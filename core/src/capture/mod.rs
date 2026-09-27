//! Screen capture per-platform, diunifikasi lewat trait [`ScreenCapturer`].
//!
//! - Windows: `Windows.Graphics.Capture` via crate `windows-capture`
//!   (dibangun di atas `windows-rs`)
//! - macOS: ScreenCaptureKit via `objc2-screen-capture-kit`
//!
//! Format pixel yang distandarkan adalah [`PixelFormat::Bgra8`] — format
//! native kedua API (D3D11 `B8G8R8A8_UNORM` dan SCK
//! `kCVPixelFormatType_32BGRA`), sehingga tidak ada konversi sebelum
//! masuk encoder H.264 di T2.

use std::fmt;

/// Format pixel frame mentah. T1 hanya mendukung BGRA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 4 byte per piksel, urutan memori B-G-R-A.
    Bgra8,
}

impl PixelFormat {
    /// Jumlah byte per piksel untuk format ini.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Bgra8 => 4,
        }
    }
}

/// Satu frame hasil capture: pixel mentah plus dimensinya.
///
/// `stride` adalah jumlah byte per baris dan bisa lebih besar dari
/// `width * bytes_per_pixel` karena padding/alignment platform.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

impl Frame {
    /// Validasi dasar konsistensi dimensi vs panjang buffer.
    pub fn validate(&self) -> Result<(), CaptureError> {
        let expected = self.stride as usize * self.height as usize;
        if self.data.len() < expected {
            return Err(CaptureError::Frame(format!(
                "buffer terlalu kecil: {} byte untuk {}x{} stride {} (butuh {})",
                self.data.len(),
                self.width,
                self.height,
                self.stride,
                expected
            )));
        }
        if self.stride < self.width * self.format.bytes_per_pixel() {
            return Err(CaptureError::Frame(format!(
                "stride {} lebih kecil dari lebar baris {}",
                self.stride,
                self.width * self.format.bytes_per_pixel()
            )));
        }
        Ok(())
    }
}

/// Error terunifikasi untuk semua backend capture.
#[derive(Debug)]
#[non_exhaustive]
pub enum CaptureError {
    /// Izin screen recording/capture belum diberikan user.
    PermissionDenied(String),
    /// Tidak ada display utama yang bisa di-capture.
    NoDisplay(String),
    /// Gagal setup/teardown session capture.
    Session(String),
    /// Gagal mengambil atau memvalidasi frame.
    Frame(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::PermissionDenied(msg) => write!(f, "izin capture ditolak: {msg}"),
            CaptureError::NoDisplay(msg) => write!(f, "display tidak tersedia: {msg}"),
            CaptureError::Session(msg) => write!(f, "session capture gagal: {msg}"),
            CaptureError::Frame(msg) => write!(f, "frame capture gagal: {msg}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Interface generik capture layar display utama.
///
/// Implementasi per-platform hidup di modul `windows` / `macos`;
/// pemanggil (T2/T4) hanya berinteraksi lewat trait ini plus
/// [`default_capturer`], tanpa `#[cfg]` apapun.
pub trait ScreenCapturer: Sized {
    /// Inisialisasi session capture untuk display utama.
    fn new() -> Result<Self, CaptureError>;

    /// Inisialisasi session capture untuk display tertentu.
    ///
    /// Implementasi default menjaga kompatibilitas capturer test/legacy yang
    /// hanya mengenal alias `main`. Backend platform meng-override method ini
    /// untuk mendukung ID yang dikembalikan [`available_displays`].
    fn new_for_display(display_id: &str) -> Result<Self, CaptureError> {
        if display_id == "main" {
            Self::new()
        } else {
            Err(CaptureError::NoDisplay(format!(
                "display '{display_id}' tidak dikenali"
            )))
        }
    }

    /// Capture 1 frame dari display utama sebagai pixel mentah BGRA8.
    fn capture_frame(&mut self) -> Result<Frame, CaptureError>;

    /// Ukuran display utama dalam piksel, (lebar, tinggi).
    fn display_size(&self) -> (u32, u32);
}

/// Metadata display aktif yang aman dikirim ke layer UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDisplay {
    /// ID opaque untuk dipakai kembali pada [`ScreenCapturer::new_for_display`].
    pub id: String,
    /// Nama user-facing; tidak dipakai sebagai identifier.
    pub name: String,
    pub is_primary: bool,
    pub width: u32,
    pub height: u32,
}

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
pub mod macos;

/// Bangun capturer untuk platform yang sedang berjalan.
///
/// Ini satu-satunya titik yang tahu soal platform; seluruh kode
/// downstream cukup memanggil fungsi ini dan memakai trait
/// [`ScreenCapturer`] secara generik.
#[cfg(target_os = "windows")]
pub fn default_capturer() -> Result<windows::WindowsCapturer, CaptureError> {
    windows::WindowsCapturer::new()
}

#[cfg(target_os = "windows")]
pub fn capturer_for_display(display_id: &str) -> Result<windows::WindowsCapturer, CaptureError> {
    windows::WindowsCapturer::new_for_display(display_id)
}

#[cfg(target_os = "windows")]
pub fn available_displays() -> Result<Vec<CaptureDisplay>, CaptureError> {
    windows::available_displays()
}

/// Bangun capturer untuk platform yang sedang berjalan.
///
/// Ini satu-satunya titik yang tahu soal platform; seluruh kode
/// downstream cukup memanggil fungsi ini dan memakai trait
/// [`ScreenCapturer`] secara generik.
#[cfg(target_os = "macos")]
pub fn default_capturer() -> Result<macos::MacosCapturer, CaptureError> {
    macos::MacosCapturer::new()
}

#[cfg(target_os = "macos")]
pub fn capturer_for_display(display_id: &str) -> Result<macos::MacosCapturer, CaptureError> {
    macos::MacosCapturer::new_for_display(display_id)
}

#[cfg(target_os = "macos")]
pub fn available_displays() -> Result<Vec<CaptureDisplay>, CaptureError> {
    macos::available_displays()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bukti kompilasi bahwa pemanggil generik cukup tahu trait
    /// [`ScreenCapturer`] — tanpa `#[cfg]` platform apapun. Test ini tidak
    /// menjalankan capture (butuh display & izin OS), hanya memastikan
    /// tipe yang dikembalikan [`default_capturer`] memenuhi trait.
    #[test]
    fn default_capturer_is_usable_generically() {
        fn use_generically<C: ScreenCapturer>(make: fn() -> Result<C, CaptureError>) {
            let _ = make;
        }

        use_generically(default_capturer);
    }
}
