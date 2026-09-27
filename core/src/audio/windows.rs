//! System audio capture Windows via **WASAPI loopback** (shared mode).
//!
//! Alur: default render endpoint → `IAudioClient` shared + loopback →
//! `IAudioCaptureClient` polling → normalisasi ke f32 stereo.
//!
//! Catatan penting:
//! - COM diinisialisasi **MTA** pada thread pipeline audio (thread dedikasi
//!   milik pipeline) lalu di-uninitialize saat capturer di-drop.
//! - Mix format shared-mode biasanya Float32 (bisa 44,1 kHz) → resampling ke
//!   48 kHz dilakukan `StreamResampler` di pipeline (bukan di sini).
//! - Device invalidation (`AUDCLNT_E_DEVICE_INVALIDATED`) muncul sebagai error
//!   baca; pipeline membangun ulang capturer (default endpoint di-query lagi)
//!   dengan backoff — pemulihan device/sleep/wake.
//! - Tidak ada antrean tak terbatas: satu paket per `read_frame`; polling
//!   dengan batas waktu supaya route switch tetap responsif.
//!
//! STATUS VALIDASI: implementasi ini belum dijalankan di host Windows nyata
//! (tidak tersedia di lingkungan pengembangan). Ia diverifikasi
//! `cargo check --target x86_64-pc-windows-msvc` (kompilasi tipe) tetapi
//! **belum** divalidasi runtime. Lihat catatan R4.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::GUID;

use super::pcm::{PcmSourceFormat, convert_to_stereo_f32_into};
use super::{AudioCapability, AudioCaptureError, AudioFrame, SystemAudioCapturer};

/// Format tag yang mungkin dari `GetMixFormat`.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// GUID subformat untuk WAVEFORMATEXTENSIBLE (KSDATAFORMAT_SUBTYPE_*).
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
const SUBTYPE_PCM: GUID = GUID::from_u128(0x0000_0001_0000_0010_8000_00aa_0038_9b71);

/// Batas waktu menunggu data; setelah ini `read_frame` mengembalikan Timeout
/// (dipakai pipeline untuk memeriksa stop/route).
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// Jeda polling saat belum ada paket.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Capturer loopback WASAPI.
pub struct WasapiLoopbackCapturer {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    sample_rate: u32,
    channels: u16,
    format: PcmSourceFormat,
    block_align: usize,
    /// COM di-initialize di thread ini → harus di-uninitialize saat drop.
    com_initialized: bool,
}

impl SystemAudioCapturer for WasapiLoopbackCapturer {
    fn new() -> Result<Self, AudioCaptureError> {
        Self::new_inner()
    }

    fn read_frame(&mut self) -> Result<AudioFrame, AudioCaptureError> {
        let mut out = AudioFrame::empty();
        self.read_frame_into(&mut out)?;
        Ok(out)
    }

    fn read_frame_into(&mut self, out: &mut AudioFrame) -> Result<(), AudioCaptureError> {
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            // SAFETY: antarmuka COM valid selama capturer hidup.
            let packet_frames = unsafe {
                self.capture
                    .GetNextPacketSize()
                    .map_err(|e| AudioCaptureError::Session(format!("GetNextPacketSize: {e}")))?
            };
            if packet_frames == 0 {
                if Instant::now() >= deadline {
                    return Err(AudioCaptureError::Timeout(
                        "tidak ada paket WASAPI dalam batas waktu".into(),
                    ));
                }
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }

            // SAFETY: buffer milik capture client; ReleaseBuffer setelah salin.
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| {
                        // Sering = device hilang; pipeline akan rebuild.
                        AudioCaptureError::Session(format!("GetBuffer: {e}"))
                    })?;
            }

            let byte_len = frames as usize * self.block_align;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;

            // Isi ulang `out.samples` (kapasitas dipakai ulang bila cukup).
            let convert = if silent || data.is_null() {
                out.samples.clear();
                out.samples.resize(frames as usize * 2, 0.0);
                Ok(())
            } else {
                // SAFETY: `data` menunjuk `byte_len` byte valid sampai ReleaseBuffer.
                let slice = unsafe { std::slice::from_raw_parts(data as *const u8, byte_len) };
                convert_to_stereo_f32_into(&mut out.samples, slice, self.format, self.channels)
            };

            // SAFETY: wajib dipanggil setelah membaca buffer.
            unsafe {
                self.capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| AudioCaptureError::Session(format!("ReleaseBuffer: {e}")))?;
            }
            convert?;

            out.sample_rate = self.sample_rate;
            out.channels = 2;
            // Timestamp monotonik dari durasi paket (bukan wallclock).
            out.timestamp = Duration::from_secs_f64(frames as f64 / self.sample_rate.max(1) as f64);
            return Ok(());
        }
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        2
    }
}

impl WasapiLoopbackCapturer {
    fn new_inner() -> Result<Self, AudioCaptureError> {
        // SAFETY: COM MTA pada thread ini; di-uninit saat drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com_initialized = hr == S_OK;
        if hr.is_err() {
            // RPC_E_CHANGED_MODE (sudah diinit mode lain) masih bisa lanjut
            // karena WASAPI bisa dipakai dari apartment apa pun; jangan uninit
            // (bukan kita yang menginisialisasi).
            if hr.0 as u32 != 0x8001_0106 {
                return Err(AudioCaptureError::Session(format!(
                    "CoInitializeEx gagal: {hr:?}"
                )));
            }
        }

        let result = Self::setup();
        if result.is_err() && com_initialized {
            unsafe { CoUninitialize() };
        }
        result
    }

    fn setup() -> Result<Self, AudioCaptureError> {
        // SAFETY: seluruh rantai COM mengikuti kontrak WASAPI loopback.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| AudioCaptureError::Session(format!("enumerator: {e}")))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| AudioCaptureError::NoDevice(format!("endpoint default: {e}")))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| AudioCaptureError::Session(format!("aktifkan IAudioClient: {e}")))?;

            let mix = client
                .GetMixFormat()
                .map_err(|e| AudioCaptureError::Session(format!("GetMixFormat: {e}")))?;
            if mix.is_null() {
                return Err(AudioCaptureError::Format("mix format null".into()));
            }
            let wf: WAVEFORMATEX = *mix;
            let (format, bits) = decode_format(mix);
            let block_align = wf.nBlockAlign as usize;
            let sample_rate = wf.nSamplesPerSec;
            let channels = wf.nChannels;

            // Shared mode + loopback: durasi buffer 0 = default engine.
            let init = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                0,
                0,
                mix,
                None,
            );
            CoTaskMemFree(Some(mix as *const core::ffi::c_void));
            init.map_err(|e| AudioCaptureError::Session(format!("Initialize: {e}")))?;

            if channels == 0 || sample_rate == 0 || block_align == 0 {
                return Err(AudioCaptureError::Format(format!(
                    "mix format tidak valid: {channels}ch {sample_rate}Hz {bits}bit"
                )));
            }

            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| AudioCaptureError::Session(format!("IAudioCaptureClient: {e}")))?;
            client
                .Start()
                .map_err(|e| AudioCaptureError::Session(format!("Start: {e}")))?;

            Ok(Self {
                client,
                capture,
                sample_rate,
                channels,
                format,
                block_align,
                com_initialized: true,
            })
        }
    }
}

impl Drop for WasapiLoopbackCapturer {
    fn drop(&mut self) {
        // SAFETY: hentikan stream lalu uninitialize COM bila kita yang init.
        unsafe {
            let _ = self.client.Stop();
        }
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

/// Tentukan format PCM + bit dari WAVEFORMATEX/WAVEFORMATEXTENSIBLE.
fn decode_format(mix: *const WAVEFORMATEX) -> (PcmSourceFormat, u16) {
    // SAFETY: `mix` valid (dari GetMixFormat) dan menunjuk minimal WAVEFORMATEX.
    let wf = unsafe { *mix };
    let tag = wf.wFormatTag;
    let bits = wf.wBitsPerSample;

    let effective_tag = if tag == WAVE_FORMAT_EXTENSIBLE {
        // Baca SubFormat dengan unaligned (struct packed(1)); salin ke lokal
        // agar tidak mengambil referensi ke field packed (E0793).
        let ext_ptr = mix as *const WAVEFORMATEXTENSIBLE;
        let ext = unsafe { std::ptr::read_unaligned(ext_ptr) };
        let sub: GUID = ext.SubFormat;
        if sub == SUBTYPE_IEEE_FLOAT {
            WAVE_FORMAT_IEEE_FLOAT
        } else if sub == SUBTYPE_PCM {
            WAVE_FORMAT_PCM
        } else {
            tag
        }
    } else {
        tag
    };

    if effective_tag == WAVE_FORMAT_IEEE_FLOAT {
        (PcmSourceFormat::F32Interleaved, bits)
    } else {
        // PCM integer: hanya 16-bit yang didukung (kasus umum share mode
        // 16-bit legacy). 24/32-bit integer jarang pada mix format default.
        (PcmSourceFormat::I16Interleaved, bits)
    }
}

/// Probe capability: WASAPI loopback tersedia di semua Windows yang didukung
/// (Vista+). Error runtime (device hilang) muncul saat capture dimulai.
pub fn capability() -> AudioCapability {
    AudioCapability {
        available: true,
        reason: None,
    }
}
