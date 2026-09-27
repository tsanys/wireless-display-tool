//! macOS screen capture via ScreenCaptureKit — **SCStream persisten**.
//!
//! Versi awal (T1) memakai `SCScreenshotManager` one-shot per frame. Terukur
//! di host: **~45 ms/frame** (≈22 fps ceiling) karena tiap frame menempuh
//! roundtrip window-server; itu bottleneck utama pipeline (T6).
//!
//! Sekarang: satu [`SCStream`] dibuat sekali di `new()`, dan delegate
//! `SCStreamOutput` menerima `CMSampleBuffer` secara kontinu (dijalankan di
//! queue dispatch sendiri). Frame terbaru disimpan di slot (mutex+condvar)
//! sehingga **selalu frame terbaru, memori terbatas** (tanpa antrean).
//! `capture_frame()` menunggu frame berikutnya di slot.
//!
//! Izin Screen Recording dicek dulu; tanpa izin → error jelas, bukan crash.

use std::ffi::c_void;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use core_graphics::display::CGDisplay;
use dispatch2::DispatchQueue;
use objc2::AnyThread;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGDisplayPixelsHigh, CGDisplayPixelsWide, CGMainDisplayID, CGPreflightScreenCaptureAccess,
    CGRequestScreenCaptureAccess,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA,
    kCVReturnSuccess,
};
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};

use super::{CaptureDisplay, CaptureError, Frame, PixelFormat, ScreenCapturer};

/// Slot frame terbaru (latest-wins) + sinyal untuk waiter.
struct FrameSlot {
    frame: Mutex<Option<Frame>>,
    ready: Condvar,
}

impl FrameSlot {
    fn new() -> Self {
        Self {
            frame: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn put(&self, frame: Frame) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(frame);
            self.ready.notify_one();
        }
    }

    fn take_wait(&self, timeout: Duration) -> Option<Frame> {
        let deadline = std::time::Instant::now() + timeout;
        let mut slot = self.frame.lock().ok()?;
        loop {
            if let Some(frame) = slot.take() {
                return Some(frame);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self.ready.wait_timeout(slot, deadline - now).ok()?;
            slot = guard;
        }
    }

    fn clear(&self) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = None;
        }
    }
}

/// Slot global: satu stream capture aktif per proses (didokumentasikan).
/// Pipeline membuat satu capturer per sesi mirror, dan `Drop` menghentikan
/// stream sebelum capturer berikutnya dibuat (stop_pipeline men-join thread),
/// jadi tidak ada dua stream bersamaan.
fn slot() -> &'static FrameSlot {
    static SLOT: OnceLock<FrameSlot> = OnceLock::new();
    SLOT.get_or_init(FrameSlot::new)
}

// Delegate `SCStreamOutput`: salin frame BGRA ke slot (latest-wins).
define_class!(
    // SAFETY: NSObject tidak punya subclassing requirement; kelas ini tanpa Drop.
    #[unsafe(super(NSObject))]
    #[name = "WdtStreamOutput"]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // SAFETY: sample_buffer valid selama callback; kami menyalin
            // datanya keluar (tidak menyimpan pointer).
            if let Some(frame) = unsafe { frame_from_sample_buffer(sample_buffer) } {
                slot().put(frame);
            }
        }
    }
);

impl StreamOutput {
    fn create() -> Result<Retained<Self>, CaptureError> {
        // SAFETY: ivars = () sehingga init standar NSObject cukup.
        let this: Allocated<Self> = Self::alloc();
        let obj: Retained<Self> = unsafe { msg_send![this, init] };
        Ok(obj)
    }
}

/// Salin `CMSampleBuffer` (BGRA) menjadi [`Frame`].
///
/// # Safety
///
/// `sample_buffer` harus valid (dijamin selama callback SCStream).
unsafe fn frame_from_sample_buffer(sample_buffer: &CMSampleBuffer) -> Option<Frame> {
    // SAFETY: sample_buffer valid selama callback.
    let image: CFRetained<CVPixelBuffer> = unsafe { sample_buffer.image_buffer() }?;
    let pix: &CVPixelBuffer = &image;

    // SAFETY: lock/unlock berpasangan; data disalin sebelum unlock.
    unsafe {
        if CVPixelBufferLockBaseAddress(pix, CVPixelBufferLockFlags::ReadOnly) != kCVReturnSuccess {
            return None;
        }
        let width = CVPixelBufferGetWidth(pix) as u32;
        let height = CVPixelBufferGetHeight(pix) as u32;
        let stride = CVPixelBufferGetBytesPerRow(pix) as u32;
        let base = CVPixelBufferGetBaseAddress(pix) as *const u8;
        let frame = if base.is_null() || width == 0 || height == 0 {
            None
        } else {
            let len = stride as usize * height as usize;
            let data = std::slice::from_raw_parts(base, len).to_vec();
            Some(Frame {
                width,
                height,
                stride,
                format: PixelFormat::Bgra8,
                data,
            })
        };
        CVPixelBufferUnlockBaseAddress(pix, CVPixelBufferLockFlags::ReadOnly);
        frame
    }
}

/// Batas tunggu frame dari SCStream.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Capturer display utama di macOS (SCStream persisten).
pub struct MacosCapturer {
    width: u32,
    height: u32,
    stream: Retained<SCStream>,
    /// Queue dispatch untuk delegate (harus hidup selama stream).
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
    /// Delegate harus tetap hidup (di-retain stream, tapi kita simpan juga).
    _delegate: Retained<StreamOutput>,
}

impl ScreenCapturer for MacosCapturer {
    fn new() -> Result<Self, CaptureError> {
        Self::new_with_display_id(CGMainDisplayID())
    }

    fn new_for_display(display_id: &str) -> Result<Self, CaptureError> {
        let display_id = parse_display_id(display_id)?;
        Self::new_with_display_id(display_id)
    }

    fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
        slot().take_wait(FRAME_TIMEOUT).ok_or_else(|| {
            CaptureError::Frame(format!(
                "tidak ada frame SCStream dalam {} dtk",
                FRAME_TIMEOUT.as_secs()
            ))
        })
    }

    fn display_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl MacosCapturer {
    fn new_with_display_id(display_id: u32) -> Result<Self, CaptureError> {
        if !CGPreflightScreenCaptureAccess() {
            // Memunculkan prompt sistem sekali; user menyetujui di
            // System Settings > Privacy & Security > Screen Recording.
            CGRequestScreenCaptureAccess();
            if !CGPreflightScreenCaptureAccess() {
                return Err(CaptureError::PermissionDenied(
                    "Screen Recording belum diizinkan. Aktifkan untuk app ini di \
                     System Settings > Privacy & Security > Screen Recording, \
                     lalu jalankan ulang."
                        .to_string(),
                ));
            }
        }

        let active = CGDisplay::active_displays()
            .map_err(|e| CaptureError::Session(format!("gagal membaca daftar display: {e}")))?;
        if !active.contains(&display_id) {
            return Err(CaptureError::NoDisplay(format!(
                "display cg:{display_id} sudah tidak terhubung"
            )));
        }

        let width = CGDisplayPixelsWide(display_id) as u32;
        let height = CGDisplayPixelsHigh(display_id) as u32;
        if width == 0 || height == 0 {
            return Err(CaptureError::NoDisplay(format!(
                "display cg:{display_id} melaporkan ukuran 0x0"
            )));
        }

        // Bersihkan sisa frame sesi sebelumnya.
        slot().clear();

        // SAFETY: seluruh blok mengikuti kontrak SCK standar.
        unsafe {
            let content = get_shareable_content()?;
            let display = find_display(&content, display_id)?;

            let empty: Retained<NSArray<SCWindow>> = NSArray::from_slice(&[]);
            let filter: Retained<SCContentFilter> = msg_send![
                SCContentFilter::alloc(),
                initWithDisplay: &*display,
                excludingWindows: &*empty,
            ];

            let config = SCStreamConfiguration::new();
            config.setWidth(width as usize);
            config.setHeight(height as usize);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setShowsCursor(true);
            // queueDepth kecil: kami hanya butuh frame terbaru (low latency).
            config.setQueueDepth(3);
            // Throttle ke ~30 fps (default 60).
            config.setMinimumFrameInterval(objc2_core_media::CMTime::new(1, 30));

            let delegate = StreamOutput::create()?;
            let stream: Retained<SCStream> = msg_send![
                SCStream::alloc(),
                initWithFilter: &*filter,
                configuration: &*config,
                delegate: std::ptr::null::<ProtocolObject<dyn objc2_screen_capture_kit::SCStreamDelegate>>(),
            ];

            let queue = DispatchQueue::new("wdt.capture", None);
            let output: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*delegate);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    output,
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e: Retained<NSError>| {
                    CaptureError::Session(format!("addStreamOutput gagal: {e}"))
                })?;

            stream.startCaptureWithCompletionHandler(None);

            Ok(Self {
                width,
                height,
                stream,
                _queue: queue,
                _delegate: delegate,
            })
        }
    }
}

/// Enumerasi display aktif. `cg:<id>` bersifat opaque bagi UI dan selalu
/// divalidasi ulang ketika sesi dimulai, sehingga hot-unplug menghasilkan
/// error yang jelas alih-alih diam-diam merekam monitor lain.
pub fn available_displays() -> Result<Vec<CaptureDisplay>, CaptureError> {
    let main_id = CGMainDisplayID();
    let ids = CGDisplay::active_displays()
        .map_err(|e| CaptureError::Session(format!("gagal membaca daftar display: {e}")))?;
    if ids.is_empty() {
        return Err(CaptureError::NoDisplay(
            "tidak ada display aktif".to_string(),
        ));
    }

    Ok(ids
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let is_primary = id == main_id;
            CaptureDisplay {
                id: format!("cg:{id}"),
                name: if is_primary {
                    "Layar utama".to_string()
                } else {
                    format!("Layar {}", index + 1)
                },
                is_primary,
                width: CGDisplayPixelsWide(id) as u32,
                height: CGDisplayPixelsHigh(id) as u32,
            }
        })
        .collect())
}

fn parse_display_id(display_id: &str) -> Result<u32, CaptureError> {
    if display_id == "main" {
        return Ok(CGMainDisplayID());
    }
    display_id
        .strip_prefix("cg:")
        .and_then(|id| id.parse::<u32>().ok())
        .ok_or_else(|| CaptureError::NoDisplay(format!("ID display '{display_id}' tidak valid")))
}

impl Drop for MacosCapturer {
    fn drop(&mut self) {
        // SAFETY: menghentikan stream milik sendiri; idempoten.
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
        }
    }
}

/// Ambil [`SCShareableContent`] secara sinkron (hanya saat setup).
fn get_shareable_content() -> Result<Retained<SCShareableContent>, CaptureError> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<Retained<SCShareableContent>, String>>();
    // Rc (bukan Arc): block hanya dipanggil pada thread yang memompa runloop
    // di fungsi ini, jadi tidak perlu Send/Sync.
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let block = block2::StackBlock::new({
        let tx = std::rc::Rc::clone(&tx);
        move |content: *mut SCShareableContent, err: *mut NSError| {
            let result = if content.is_null() {
                Err(unsafe { err.as_ref() }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "unknown error".to_string()))
            } else {
                // SAFETY: SCK menjamin objek valid saat pointer non-null.
                Ok(unsafe { Retained::retain(content).unwrap() })
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(result);
            }
        }
    });
    let block_ref: &block2::Block<dyn Fn(*mut SCShareableContent, *mut NSError)> = &block;

    // SAFETY: block hidup sampai pump selesai.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            false, true, block_ref,
        );
    }
    pump_until(&rx, "getShareableContent")?.map_err(CaptureError::Session)
}

/// Cari [`SCDisplay`] dengan displayID yang dipilih.
fn find_display(
    content: &SCShareableContent,
    display_id: u32,
) -> Result<Retained<SCDisplay>, CaptureError> {
    // SAFETY: accessor ObjC standar pada objek hasil query SCK.
    unsafe {
        let displays = content.displays();
        for i in 0..displays.count() {
            let display = displays.objectAtIndex(i);
            if display.displayID() == display_id {
                return Ok(display);
            }
        }
    }
    Err(CaptureError::NoDisplay(format!(
        "display cg:{display_id} tidak ada di shareable content"
    )))
}

/// Pompa runloop sampai `rx` terisi (dipakai hanya untuk setup).
fn pump_until<T>(rx: &std::sync::mpsc::Receiver<T>, what: &str) -> Result<T, CaptureError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(v) = rx.try_recv() {
            return Ok(v);
        }
        if std::time::Instant::now() >= deadline {
            return Err(CaptureError::Session(format!("{what} timeout")));
        }
        // SAFETY: membaca extern static Apple yang selalu valid.
        let mode = unsafe { objc2_core_foundation::kCFRunLoopDefaultMode };
        objc2_core_foundation::CFRunLoop::run_in_mode(mode, 0.001, false);
    }
}

/// Re-export minimum agar `NSString`/`c_void` tidak "unused" bila cfg lain.
#[allow(dead_code)]
fn _type_anchors(_: *const c_void, _: *const NSString) {}
