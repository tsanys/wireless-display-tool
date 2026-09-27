//! System audio capture macOS via ScreenCaptureKit — **SCStream khusus
//! audio** (tanpa output video).
//!
//! Kenapa stream terpisah (bukan menumpang SCStream video):
//! - lifecycle audio independen — route switch dan recovery tidak menyentuh
//!   pipeline video (objek `SCStream` tidak `Send`, jadi memakai ulang
//!   stream milik thread video akan mengikat dua lifecycle);
//! - SCK sudah melakukan resampling/konversi kanal ke `sampleRate` dan
//!   `channelCount` yang diminta, jadi kita minta langsung **48 kHz stereo**
//!   dan tidak butuh resampler di jalur macOS (passthrough).
//!
//! Format yang SCK sampaikan adalah Format standar CoreAudio: **Float32**
//! (bisa interleaved, bisa de-interleaved/planar); kita normalkan lewat
//! `pcm::convert_to_stereo_f32`.
//!
//! Ketersediaan: capture audio sistem SCK memerlukan **macOS 13.0+** dan
//! izin **Screen Recording** (TCC yang sama dengan capture layar).
//! `excludesCurrentProcessAudio` (14.0+) hanya untuk mengecualikan audio
//! proses sendiri (kita tidak memutar audio, jadi tidak krusial).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::AnyThread;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsSignedInteger,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMSampleBuffer,
    kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
};
use objc2_foundation::{NSArray, NSError, NSProcessInfo};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};

use super::pcm::{PcmSourceFormat, convert_to_stereo_f32};
use super::{AudioCapability, AudioCaptureError, AudioFrame, OUT_SAMPLE_RATE, SystemAudioCapturer};

/// Batas tunggu satu chunk dari SCK.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// Kapasitas antrean bounded; kelebihan → buang yang tertua (realtime).
const MAX_QUEUED: usize = 32;

/// Antrean audio bounded (drop-oldest) + sinyal untuk waiter.
struct AudioSlot {
    queue: Mutex<VecDeque<AudioFrame>>,
    ready: Condvar,
}

impl AudioSlot {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(MAX_QUEUED)),
            ready: Condvar::new(),
        }
    }

    fn put(&self, frame: AudioFrame) {
        if let Ok(mut q) = self.queue.lock() {
            // Backpressure: jangan menumpuk — buang data paling basi.
            while q.len() >= MAX_QUEUED {
                q.pop_front();
            }
            q.push_back(frame);
            self.ready.notify_one();
        }
    }

    fn take_wait(&self, timeout: Duration) -> Option<AudioFrame> {
        let deadline = std::time::Instant::now() + timeout;
        let mut q = self.queue.lock().ok()?;
        loop {
            if let Some(frame) = q.pop_front() {
                return Some(frame);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self.ready.wait_timeout(q, deadline - now).ok()?;
            q = guard;
        }
    }

    fn clear(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.clear();
        }
    }
}

/// Slot global: satu stream audio aktif per proses (pola sama dengan video).
fn slot() -> &'static AudioSlot {
    static SLOT: OnceLock<AudioSlot> = OnceLock::new();
    SLOT.get_or_init(AudioSlot::new)
}

/// Counter diagnostik delegate (untuk probe/verifikasi capture).
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static BUILT: AtomicU64 = AtomicU64::new(0);
static BUILD_FAILED: AtomicU64 = AtomicU64::new(0);
/// Tahap kegagalan frame (lihat `frame_from_audio_sample_buffer`).
static STAGE: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn stage_fail(i: usize) {
    if let Some(c) = STAGE.get(i) {
        c.fetch_add(1, Ordering::Relaxed);
    }
}

/// Counter kegagalan per tahap: [fmt_none, asbd, abl1, abl2, data_kosong,
/// format_tak_didukung, konversi, unknown].
pub fn debug_stages() -> [u64; 8] {
    STAGE.each_ref().map(|c| c.load(Ordering::Relaxed))
}

// Delegate `SCStreamOutput`: salin chunk audio ke slot (drop-oldest).
define_class!(
    // SAFETY: NSObject tanpa subclassing requirement; kelas ini tanpa Drop.
    #[unsafe(super(NSObject))]
    #[name = "WdtAudioOutput"]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: sample_buffer valid selama callback; data disalin keluar.
            match unsafe { frame_from_audio_sample_buffer(sample_buffer) } {
                Some(frame) => {
                    BUILT.fetch_add(1, Ordering::Relaxed);
                    slot().put(frame);
                }
                None => {
                    BUILD_FAILED.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
);

/// Counter diagnostik: (callback audio, frame berhasil, frame gagal dibangun).
pub fn debug_stats() -> (u64, u64, u64) {
    (
        CALLBACKS.load(Ordering::Relaxed),
        BUILT.load(Ordering::Relaxed),
        BUILD_FAILED.load(Ordering::Relaxed),
    )
}

impl AudioOutput {
    fn create() -> Result<Retained<Self>, AudioCaptureError> {
        // SAFETY: ivars = () sehingga init standar NSObject cukup.
        let this: Allocated<Self> = Self::alloc();
        let obj: Retained<Self> = unsafe { msg_send![this, init] };
        Ok(obj)
    }
}

/// Ekstrak [`AudioFrame`] (f32 stereo) dari CMSampleBuffer audio SCK.
///
/// # Safety
/// `sample_buffer` harus valid (dijamin selama callback SCStream).
unsafe fn frame_from_audio_sample_buffer(sample_buffer: &CMSampleBuffer) -> Option<AudioFrame> {
    // 1. ASBD: sample rate, kanal, format (float/int, interleaved/planar).
    let fmt = match unsafe { sample_buffer.format_description() } {
        Some(f) => f,
        None => {
            stage_fail(0);
            return None;
        }
    };
    let asbd_ptr: *const AudioStreamBasicDescription =
        unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&fmt) };
    if asbd_ptr.is_null() {
        stage_fail(1);
        return None;
    }
    let asbd = unsafe { &*asbd_ptr };
    let sample_rate = asbd.mSampleRate.round() as u32;
    if sample_rate == 0 {
        stage_fail(1);
        return None;
    }
    let channels = asbd.mChannelsPerFrame as u16;
    if channels == 0 {
        stage_fail(1);
        return None;
    }
    let is_float = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0;
    let is_int = asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0;

    // 2. AudioBufferList (dua panggilan API: ukur lalu isi).
    //
    // Mengikuti pola kanonik contoh ScreenCaptureKit Apple: block buffer
    // out HARUS diberi pointer valid (bukan null) pada panggilan kedua, dan
    // buffer AudioBufferList di-align 16 byte saat memakai flag
    // Assure16ByteAlignment.
    let flags = kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment;
    let mut needed: usize = 0;
    // SAFETY: panggilan ukur; buffer_list_out null → hanya mengisi `needed`.
    let status = unsafe {
        sample_buffer.audio_buffer_list_with_retained_block_buffer(
            &mut needed as *mut usize,
            std::ptr::null_mut(),
            0,
            None,
            None,
            flags,
            std::ptr::null_mut(),
        )
    };
    if status != 0 || needed == 0 {
        stage_fail(2);
        return None;
    }
    // Backing align 16 byte (u128); CoreMedia mengisi sesuai `needed`.
    let mut backing: Vec<u128> = vec![0; needed.div_ceil(16)];
    let abl = backing.as_mut_ptr() as *mut AudioBufferList;
    let mut block_buffer: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: `abl` menunjuk `needed` byte 16-byte aligned; data valid selama
    // sample_buffer hidup; block buffer dikembalikan retained (kita lepas).
    let status = unsafe {
        sample_buffer.audio_buffer_list_with_retained_block_buffer(
            std::ptr::null_mut(),
            abl,
            needed,
            None,
            None,
            flags,
            &mut block_buffer as *mut *mut CMBlockBuffer,
        )
    };
    if status != 0 {
        if !block_buffer.is_null() {
            drop(unsafe { CFRetained::from_raw(std::ptr::NonNull::new_unchecked(block_buffer)) });
        }
        stage_fail(3);
        return None;
    }

    let num_buffers = unsafe { (*abl).mNumberBuffers } as usize;
    let mut data: Vec<u8> = Vec::new();
    let buffers_ptr = unsafe { (*abl).mBuffers.as_ptr() };
    for i in 0..num_buffers {
        // SAFETY: mBuffers adalah flexible array; akses i < mNumberBuffers.
        let buf = unsafe { &*buffers_ptr.add(i) };
        let len = buf.mDataByteSize as usize;
        if buf.mData.is_null() || len == 0 {
            continue;
        }
        // SAFETY: mData menunjuk `len` byte milik block buffer/sample buffer.
        let slice = unsafe { std::slice::from_raw_parts(buf.mData as *const u8, len) };
        data.extend_from_slice(slice);
    }
    // Lepaskan block buffer retained (data sudah disalin).
    if !block_buffer.is_null() {
        drop(unsafe { CFRetained::from_raw(std::ptr::NonNull::new_unchecked(block_buffer)) });
    }
    if data.is_empty() {
        stage_fail(4);
        return None;
    }

    // 3. Normalisasi → f32 stereo.
    //    >1 buffer = satu kanal per buffer (planar/de-interleaved).
    let source = if is_float {
        if num_buffers > 1 {
            PcmSourceFormat::F32Planar
        } else {
            PcmSourceFormat::F32Interleaved
        }
    } else if is_int && asbd.mBitsPerChannel == 16 {
        PcmSourceFormat::I16Interleaved
    } else {
        // Format langka (fixed-point lain); tidak didukung.
        stage_fail(5);
        return None;
    };
    let samples = match convert_to_stereo_f32(&data, source, channels) {
        Ok(s) => s,
        Err(_) => {
            stage_fail(6);
            return None;
        }
    };

    // 4. Timestamp monotonik dari PTS (untuk drift-check).
    let pts = unsafe { sample_buffer.presentation_time_stamp().seconds() };
    let timestamp = if pts.is_finite() && pts >= 0.0 {
        Duration::from_secs_f64(pts)
    } else {
        Duration::ZERO
    };

    Some(AudioFrame {
        samples,
        sample_rate,
        channels: 2,
        timestamp,
    })
}

/// Capturer audio sistem macOS (SCStream audio-only).
pub struct MacosAudioCapturer {
    stream: Retained<SCStream>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
    _delegate: Retained<AudioOutput>,
}

impl SystemAudioCapturer for MacosAudioCapturer {
    fn new() -> Result<Self, AudioCaptureError> {
        Self::new_inner()
    }

    fn read_frame(&mut self) -> Result<AudioFrame, AudioCaptureError> {
        slot().take_wait(READ_TIMEOUT).ok_or_else(|| {
            AudioCaptureError::Timeout(format!(
                "tidak ada audio SCK dalam {} ms",
                READ_TIMEOUT.as_millis()
            ))
        })
    }

    fn sample_rate(&self) -> u32 {
        OUT_SAMPLE_RATE
    }

    fn channels(&self) -> u16 {
        2
    }
}

impl MacosAudioCapturer {
    fn new_inner() -> Result<Self, AudioCaptureError> {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
            if !CGPreflightScreenCaptureAccess() {
                return Err(AudioCaptureError::PermissionDenied(
                    "Screen Recording belum diizinkan. Aktifkan untuk app ini di \
                     System Settings > Privacy & Security > Screen Recording, \
                     lalu jalankan ulang."
                        .to_string(),
                ));
            }
        }

        let display_id = CGMainDisplayID();
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
            // Audio: minta langsung 48 kHz stereo — SCK yang meresample.
            config.setCapturesAudio(true);
            config.setSampleRate(OUT_SAMPLE_RATE as isize);
            config.setChannelCount(2);
            // macOS 14+: kecualikan audio proses sendiri (kita tidak memutar
            // media, jadi ini opsional dan dijaga agar tidak memanggil
            // selector yang belum ada di macOS 13).
            if macos_version().0 >= 14 {
                config.setExcludesCurrentProcessAudio(false);
            }
            // Ukuran video tidak dipakai (audio-only) tetapi SCK tetap
            // memerlukan dimensi yang valid; pakai ukuran display.
            let (dw, dh) = (display.width() as usize, display.height() as usize);
            if dw > 0 && dh > 0 {
                config.setWidth(dw);
                config.setHeight(dh);
            }
            config.setQueueDepth(5);

            let delegate = AudioOutput::create()?;
            let stream: Retained<SCStream> = msg_send![
                SCStream::alloc(),
                initWithFilter: &*filter,
                configuration: &*config,
                delegate: std::ptr::null::<ProtocolObject<dyn objc2_screen_capture_kit::SCStreamDelegate>>(),
            ];

            let queue = DispatchQueue::new("wdt.audio", None);
            let output: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*delegate);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    output,
                    SCStreamOutputType::Audio,
                    Some(&queue),
                )
                .map_err(|e: Retained<NSError>| {
                    AudioCaptureError::Session(format!("addStreamOutput audio gagal: {e}"))
                })?;

            stream.startCaptureWithCompletionHandler(None);

            Ok(Self {
                stream,
                _queue: queue,
                _delegate: delegate,
            })
        }
    }
}

impl Drop for MacosAudioCapturer {
    fn drop(&mut self) {
        // SAFETY: menghentikan stream milik sendiri; idempoten.
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
        }
        slot().clear();
    }
}

/// Versi macOS berjalan.
fn macos_version() -> (i64, i64, i64) {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    (
        v.majorVersion as i64,
        v.minorVersion as i64,
        v.patchVersion as i64,
    )
}

/// Probe capability: macOS 13+ dan izin Screen Recording tersedia.
pub fn capability() -> AudioCapability {
    let (major, minor, _) = macos_version();
    if (major, minor) < (13, 0) {
        return AudioCapability {
            available: false,
            reason: Some(format!(
                "Capture audio sistem memerlukan macOS 13 (Ventura) atau lebih baru \
                 (saat ini {major}.{minor})"
            )),
        };
    }
    // SAFETY: panggilan preflight standar.
    if !CGPreflightScreenCaptureAccess() {
        return AudioCapability {
            available: false,
            reason: Some(
                "Izin Screen Recording belum diberikan untuk WDT. Aktifkan di \
                 System Settings > Privacy & Security > Screen Recording, lalu \
                 jalankan ulang."
                    .to_string(),
            ),
        };
    }
    AudioCapability {
        available: true,
        reason: None,
    }
}

/// Ambil [`SCShareableContent`] secara sinkron (hanya saat setup).
fn get_shareable_content() -> Result<Retained<SCShareableContent>, AudioCaptureError> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<Retained<SCShareableContent>, String>>();
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
    pump_until(&rx, "getShareableContent")?.map_err(AudioCaptureError::Session)
}

/// Cari [`SCDisplay`] dengan displayID yang dipilih.
fn find_display(
    content: &SCShareableContent,
    display_id: u32,
) -> Result<Retained<SCDisplay>, AudioCaptureError> {
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
    Err(AudioCaptureError::NoDevice(format!(
        "display cg:{display_id} tidak ada di shareable content"
    )))
}

/// Pompa runloop sampai `rx` terisi (dipakai hanya untuk setup).
fn pump_until<T>(rx: &std::sync::mpsc::Receiver<T>, what: &str) -> Result<T, AudioCaptureError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(v) = rx.try_recv() {
            return Ok(v);
        }
        if std::time::Instant::now() >= deadline {
            return Err(AudioCaptureError::Session(format!("{what} timeout")));
        }
        // SAFETY: membaca extern static Apple yang selalu valid.
        let mode = unsafe { objc2_core_foundation::kCFRunLoopDefaultMode };
        objc2_core_foundation::CFRunLoop::run_in_mode(mode, 0.001, false);
    }
}
