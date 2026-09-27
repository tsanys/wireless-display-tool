//! Windows screen capture via `Windows.Graphics.Capture`.
//!
//! Dibangun di atas crate [`windows_capture`] (yang sendiri memakai
//! `windows-rs`), backend `GraphicsCaptureApi` dengan format
//! [`ColorFormat::Bgra8`](windows_capture::settings::ColorFormat).
//!
//! Session capture berjalan di thread internal `windows-capture`;
//! setiap frame yang tiba disalin ke buffer milik Rust lalu dikirim
//! lewat channel ke [`WindowsCapturer::capture_frame`]. Desain ini
//! sekaligus fondasi untuk streaming kontinu di T2.
//!
//! Batasan T1: satu session capture per proses (channel dipegang via
//! static slot karena `Flags = ()` tidak membawa state). Multi-session
//! ditangani di T2 bila dibutuhkan.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WcFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use super::{CaptureDisplay, CaptureError, Frame, PixelFormat, ScreenCapturer};

/// Batas tunggu frame pertama/setelahnya dari session capture.
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);

/// Slot channel untuk handler (lihat batasan satu session di atas).
static FRAME_TX: OnceLock<Mutex<Option<Sender<RawFrame>>>> = OnceLock::new();

/// Frame mentah yang sudah disalin keluar dari callback capture.
struct RawFrame {
    width: u32,
    height: u32,
    stride: u32,
    pixels: Vec<u8>,
}

/// Handler `windows-capture`: salin tiap frame lalu kirim ke channel.
struct FramePump;

impl GraphicsCaptureApiHandler for FramePump {
    type Flags = ();
    type Error = String;

    fn new(_ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        if FRAME_TX.get().is_none() {
            return Err("FRAME_TX belum dipasang (bug internal)".to_string());
        }
        Ok(Self)
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WcFrame<'_>,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if frame.color_format() != ColorFormat::Bgra8 {
            return Err(format!(
                "format frame tak terduga: {:?}, diharapkan Bgra8",
                frame.color_format()
            ));
        }
        let mut buffer = frame
            .buffer()
            .map_err(|e| format!("gagal ambil buffer frame: {e}"))?;
        let raw = RawFrame {
            width: buffer.width(),
            height: buffer.height(),
            stride: buffer.row_pitch(),
            pixels: buffer.as_raw_buffer().to_vec(),
        };
        // Receiver mungkin sudah pergi (teardown); abaikan error kirim.
        if let Some(slot) = FRAME_TX.get() {
            if let Some(tx) = slot.lock().unwrap().as_ref() {
                let _ = tx.send(raw);
            }
        }
        Ok(())
    }
}

/// Capturer display utama di Windows.
pub struct WindowsCapturer {
    width: u32,
    height: u32,
    rx: Receiver<RawFrame>,
    // Menahan session capture tetap hidup selama struct ini ada.
    _control: CaptureControl<FramePump, String>,
}

impl ScreenCapturer for WindowsCapturer {
    fn new() -> Result<Self, CaptureError> {
        let monitor = Monitor::primary()
            .map_err(|e| CaptureError::NoDisplay(format!("display utama tidak ditemukan: {e}")))?;
        Self::new_with_monitor(monitor)
    }

    fn new_for_display(display_id: &str) -> Result<Self, CaptureError> {
        let monitor = find_monitor(display_id)?;
        Self::new_with_monitor(monitor)
    }

    fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
        let raw = self.rx.recv_timeout(FRAME_TIMEOUT).map_err(|_| {
            CaptureError::Frame(format!(
                "tidak ada frame dalam {} detik",
                FRAME_TIMEOUT.as_secs()
            ))
        })?;
        let frame = Frame {
            width: raw.width,
            height: raw.height,
            stride: raw.stride,
            format: PixelFormat::Bgra8,
            data: raw.pixels,
        };
        frame.validate()?;
        Ok(frame)
    }

    fn display_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl WindowsCapturer {
    fn new_with_monitor(monitor: Monitor) -> Result<Self, CaptureError> {
        let width = monitor
            .width()
            .map_err(|e| CaptureError::Session(format!("gagal baca lebar display: {e}")))?;
        let height = monitor
            .height()
            .map_err(|e| CaptureError::Session(format!("gagal baca tinggi display: {e}")))?;

        let (tx, rx) = mpsc::channel::<RawFrame>();
        let frame_tx = FRAME_TX.get_or_init(|| Mutex::new(None));
        *frame_tx
            .lock()
            .map_err(|_| CaptureError::Session("slot frame terkunci".to_string()))? = Some(tx);

        let settings = Settings::new(
            monitor,
            // Kursor harus ikut tercapture agar terlihat di TV.
            CursorCaptureSettings::WithCursor,
            // Tanpa border kuning di sekitar area capture.
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            (),
        );

        let control = FramePump::start_free_threaded(settings)
            .map_err(|e| CaptureError::Session(format!("gagal start capture: {e}")))?;

        Ok(Self {
            width,
            height,
            rx,
            _control: control,
        })
    }
}

pub fn available_displays() -> Result<Vec<CaptureDisplay>, CaptureError> {
    let primary_name = Monitor::primary()
        .and_then(|monitor| monitor.device_name())
        .map_err(|e| CaptureError::NoDisplay(format!("display utama tidak ditemukan: {e}")))?;
    let monitors = Monitor::enumerate()
        .map_err(|e| CaptureError::Session(format!("gagal membaca daftar display: {e}")))?;
    if monitors.is_empty() {
        return Err(CaptureError::NoDisplay(
            "tidak ada display aktif".to_string(),
        ));
    }

    monitors
        .into_iter()
        .map(|monitor| {
            let device_name = monitor
                .device_name()
                .map_err(|e| CaptureError::Session(format!("gagal membaca ID display: {e}")))?;
            let is_primary = device_name == primary_name;
            let name = monitor.name().unwrap_or_else(|_| {
                if is_primary {
                    "Layar utama".to_string()
                } else {
                    device_name.clone()
                }
            });
            Ok(CaptureDisplay {
                id: format!("win:{device_name}"),
                name: if is_primary {
                    format!("{name} (utama)")
                } else {
                    name
                },
                is_primary,
                width: monitor.width().map_err(|e| {
                    CaptureError::Session(format!("gagal membaca lebar display: {e}"))
                })?,
                height: monitor.height().map_err(|e| {
                    CaptureError::Session(format!("gagal membaca tinggi display: {e}"))
                })?,
            })
        })
        .collect()
}

fn find_monitor(display_id: &str) -> Result<Monitor, CaptureError> {
    if display_id == "main" {
        return Monitor::primary()
            .map_err(|e| CaptureError::NoDisplay(format!("display utama tidak ditemukan: {e}")));
    }

    let wanted = display_id
        .strip_prefix("win:")
        .ok_or_else(|| CaptureError::NoDisplay(format!("ID display '{display_id}' tidak valid")))?;
    Monitor::enumerate()
        .map_err(|e| CaptureError::Session(format!("gagal membaca daftar display: {e}")))?
        .into_iter()
        .find(|monitor| {
            monitor
                .device_name()
                .map(|name| name == wanted)
                .unwrap_or(false)
        })
        .ok_or_else(|| CaptureError::NoDisplay(format!("display '{wanted}' sudah tidak terhubung")))
}
