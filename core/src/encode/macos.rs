//! macOS H.264 hardware encoding via VideoToolbox.
//!
//! Session [`VTCompressionSession`] dikonfigurasi realtime, profile Main
//! (atau Baseline), CBR, tanpa frame reordering. Input BGRA langsung
//! (nol konversi dari [`Frame`](crate::capture::Frame) T1). Output
//! di-callback C dikumpulkan jadi paket Annex B; SPS/PPS disisipkan
//! sebelum tiap IDR agar bitstream decodable standalone.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMSampleAttachmentKey_DependsOnOthers,
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreateWithBytes, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSession, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_H264EntropyMode, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_Quality,
    kVTCompressionPropertyKey_RealTime, kVTH264EntropyMode_CABAC, kVTH264EntropyMode_CAVLC,
    kVTProfileLevel_H264_Baseline_AutoLevel, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_H264_Main_AutoLevel,
};

use super::{
    EncodeError, EncodedPacket, EncoderConfig, FrameEncoder, H264EntropyMode, H264Profile,
};
use crate::capture::Frame;

/// Batas tunggu hasil encode satu frame.
const ENCODE_TIMEOUT: Duration = Duration::from_secs(10);

/// Start code Annex B.
const ANNEXB_START: [u8; 4] = [0, 0, 0, 1];

/// Hasil encode satu frame dari callback C.
enum EncodeOutcome {
    Packets(Vec<EncodedPacketPayload>),
    Failed(String),
}

struct EncodedPacketPayload {
    annexb: Vec<u8>,
    is_keyframe: bool,
}

/// Cast referensi CF apa pun ke `&CFType` (toll-free bridging: semua tipe
/// CF berbagi representasi pointer yang sama di ABI).
fn as_cftype<T>(v: &T) -> &CFType {
    // SAFETY: cast pointer identitas antar tipe CF — sound karena C API
    // hanya memakai pointer sebagai referensi opaque.
    unsafe { &*(v as *const T as *const CFType) }
}

fn as_session(s: &VTCompressionSession) -> &VTSession {
    // SAFETY: VTCompressionSession adalah CFType di ABI (toll-free).
    unsafe { &*(s as *const VTCompressionSession as *const VTSession) }
}

fn check_status(status: i32, what: &str) -> Result<(), EncodeError> {
    if status == 0 {
        Ok(())
    } else {
        Err(EncodeError::Session(format!(
            "{what} gagal (OSStatus {status})"
        )))
    }
}

/// Callback C dari VTCompressionSession. `source_refcon` adalah pointer ke
/// `Box<Sender<EncodeOutcome>>` yang ditanam saat EncodeFrame; callback
/// mengambil alih ownership-nya tepat sekali.
unsafe extern "C-unwind" fn vt_output_callback(
    _refcon: *mut c_void,
    source_refcon: *mut c_void,
    status: i32,
    _info_flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: VT menjamin source_refcon sama dengan yang diberikan saat
    // EncodeFrame, dan callback dipanggil tepat sekali per frame.
    let sender: Box<Sender<EncodeOutcome>> =
        unsafe { Box::from_raw(source_refcon as *mut Sender<EncodeOutcome>) };

    if status != 0 {
        let _ = sender.send(EncodeOutcome::Failed(format!(
            "VT encode gagal (OSStatus {status})"
        )));
        return;
    }
    if sample.is_null() {
        let _ = sender.send(EncodeOutcome::Failed("sample buffer null".to_string()));
        return;
    }
    // SAFETY: sample valid selama callback berjalan.
    let outcome = unsafe {
        sample
            .as_ref()
            .map(|s| parse_sample(s))
            .unwrap_or_else(|| EncodeOutcome::Failed("sample null".to_string()))
    };
    let _ = sender.send(outcome);
}

/// Salin baris BGRA dari [`Frame`] ke pixel buffer yang bisa **dipakai ulang**.
///
/// Pixel buffer milik encoder dibuat sekali (membungkus `pixels: Vec<u8>` yang
/// juga milik encoder) lalu diisi ulang tiap frame lewat lock+memcpy. Ini
/// menghindari alokasi/penghancuran buffer besar per frame.
///
/// KEAMANAN REUSE: `encode_frame` **menunggu** callback output VideoToolbox
/// sebelum kembali, jadi VT sudah selesai dengan pixel buffer sebelum frame
/// berikutnya menimpanya.
///
/// # Safety
/// `frame` harus berukuran sama dengan konfigurasi encoder.
unsafe fn copy_frame_into(pixel_buffer: &CVPixelBuffer, frame: &Frame) -> Result<(), EncodeError> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    // SAFETY: lock/unlock berpasangan; base valid selama lock.
    unsafe {
        let lock = CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0));
        if lock != kCVReturnSuccess {
            return Err(EncodeError::Encode(format!(
                "CVPixelBufferLockBaseAddress gagal ({lock})"
            )));
        }
        let dst = CVPixelBufferGetBaseAddress(pixel_buffer) as *mut u8;
        let dst_bpr = CVPixelBufferGetBytesPerRow(pixel_buffer);
        if dst.is_null() {
            CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0));
            return Err(EncodeError::Encode("base address pixel buffer null".into()));
        }
        let src_stride = frame.stride as usize;
        let need = src_stride
            .checked_mul(h)
            .ok_or_else(|| EncodeError::Encode("stride*height overflow".into()))?;
        if frame.data.len() < need {
            CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0));
            return Err(EncodeError::Encode(
                "buffer frame lebih kecil dari stride*height".into(),
            ));
        }
        for y in 0..h {
            let src = frame.data.as_ptr().add(y * src_stride);
            let out = dst.add(y * dst_bpr);
            std::ptr::copy_nonoverlapping(src, out, w * 4);
        }
        CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags(0));
    }
    Ok(())
}

/// Parse CMSampleBuffer H.264 (NALU length-prefixed) jadi paket Annex B.
unsafe fn parse_sample(sample: &CMSampleBuffer) -> EncodeOutcome {
    // SAFETY: seluruh akses di bawah membaca buffer yang valid selama
    // callback; pointer mentah disalin segera ke Vec.
    unsafe {
        let Some(block) = sample.data_buffer() else {
            return EncodeOutcome::Failed("sample tanpa data buffer".to_string());
        };
        let mut len_at_offset: usize = 0;
        let mut total_len: usize = 0;
        let mut data_ptr: *mut i8 = std::ptr::null_mut();
        let st = block.data_pointer(0, &mut len_at_offset, &mut total_len, &mut data_ptr);
        if st != 0 || data_ptr.is_null() {
            return EncodeOutcome::Failed(format!("CMBlockBuffer kosong (OSStatus {st})"));
        }
        let raw = std::slice::from_raw_parts(data_ptr as *const u8, total_len);

        // NALU length-prefixed (header 4 byte big-endian, standar VT).
        let mut annexb: Vec<u8> = Vec::with_capacity(total_len + 64);
        let mut off = 0usize;
        while off + 4 <= raw.len() {
            let nalu_len =
                u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]) as usize;
            off += 4;
            if off + nalu_len > raw.len() {
                return EncodeOutcome::Failed("NALU terpotong".to_string());
            }
            annexb.extend_from_slice(&ANNEXB_START);
            annexb.extend_from_slice(&raw[off..off + nalu_len]);
            off += nalu_len;
        }

        let is_keyframe = is_sync_sample(sample);
        if is_keyframe {
            // Sisipkan SPS/PPS sebelum IDR agar decodable standalone.
            if let Some(fmt) = sample.format_description() {
                let mut sps_pps = extract_sps_pps(&fmt);
                sps_pps.extend_from_slice(&annexb);
                annexb = sps_pps;
            }
        }
        EncodeOutcome::Packets(vec![EncodedPacketPayload {
            annexb,
            is_keyframe,
        }])
    }
}

/// True bila sample adalah sync sample (IDR): attachment NotSync tidak ada.
unsafe fn is_sync_sample(sample: &CMSampleBuffer) -> bool {
    // SAFETY: baca attachment read-only yang valid selama callback.
    unsafe {
        let Some(arr) = sample.sample_attachments_array(false) else {
            return false;
        };
        if arr.is_empty() {
            return false;
        }
        let dict_ptr = arr.value_at_index(0) as *const objc2_core_foundation::CFDictionary;
        let Some(dict) = dict_ptr.as_ref() else {
            return false;
        };
        // NotSync absen => sync sample (keyframe).
        let not_sync_key = kCMSampleAttachmentKey_NotSync as *const CFString as *const c_void;
        dict.value(not_sync_key).is_null() && !depends_on_others(dict)
    }
}

/// Pastikan bukan P/B-frame yang kebetulan tanpa flag NotSync.
unsafe fn depends_on_others(dict: &CFDictionary) -> bool {
    // SAFETY: baca read-only.
    unsafe {
        let depends_key =
            kCMSampleAttachmentKey_DependsOnOthers as *const CFString as *const c_void;
        let ptr = dict.value(depends_key);
        if ptr.is_null() {
            return false;
        }
        (ptr as *const CFBoolean)
            .as_ref()
            .map(|b| b.as_bool())
            .unwrap_or(false)
    }
}

/// Ambil SPS (index 0) + PPS (index 1) dari format description, format Annex B.
unsafe fn extract_sps_pps(fmt: &CMFormatDescription) -> Vec<u8> {
    // SAFETY: pointer internal valid selama format description hidup
    // (masih di dalam callback); langsung disalin ke Vec.
    unsafe {
        let mut out = Vec::new();
        for index in 0..2usize {
            let mut ptr: *const u8 = std::ptr::null();
            let mut size: usize = 0;
            let mut count: usize = 0;
            let mut hdr_len: std::ffi::c_int = 0;
            let st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                fmt,
                index,
                &mut ptr,
                &mut size,
                &mut count,
                &mut hdr_len,
            );
            if st != 0 || ptr.is_null() || size == 0 {
                break;
            }
            out.extend_from_slice(&ANNEXB_START);
            out.extend_from_slice(std::slice::from_raw_parts(ptr, size));
        }
        out
    }
}

/// Bungkus BGRA buffer encoder ke CVPixelBuffer **yang dipakai ulang**.
///
/// Pixel buffer dibuat sekali (membungkus `pixels` milik encoder), lalu tiap
/// frame hanya menimpa isinya (lihat [`copy_frame_into`]). Ini menghilangkan
/// alokasi ~8 MB per frame yang menyebabkan churn VM/swap di VideoToolbox.
unsafe fn ensure_pixel_buffer(
    pixels: &mut Vec<u8>,
    slot: &mut Option<CFRetained<CVPixelBuffer>>,
    w: usize,
    h: usize,
) -> Result<CFRetained<CVPixelBuffer>, EncodeError> {
    if let Some(pb) = slot {
        return Ok(pb.clone());
    }
    // SAFETY: base valid (Vec hidup selama encoder); release callback None
    // karena kita mengelola lifetime Vec di struct encoder.
    let base = NonNull::new(pixels.as_mut_ptr() as *mut c_void)
        .ok_or_else(|| EncodeError::Encode("alokasi pixels null".into()))?;
    let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
    let ret = unsafe {
        CVPixelBufferCreateWithBytes(
            None,
            w,
            h,
            kCVPixelFormatType_32BGRA,
            base,
            w * 4,
            None,
            std::ptr::null_mut(),
            None,
            NonNull::new(&mut out).unwrap(),
        )
    };
    if ret != kCVReturnSuccess || out.is_null() {
        return Err(EncodeError::Encode(format!(
            "CVPixelBufferCreateWithBytes gagal ({ret})"
        )));
    }
    // SAFETY: out non-null +1.
    let pb = unsafe { CFRetained::retain(NonNull::new(out).unwrap()) };
    *slot = Some(pb.clone());
    Ok(pb)
}

/// Set satu properti session; error bila OSStatus != 0.
fn set_property(
    session: &VTCompressionSession,
    key: &CFString,
    value: &CFType,
    name: &str,
) -> Result<(), EncodeError> {
    // SAFETY: key/value bertipe CF yang benar untuk properti ini.
    let st = unsafe { VTSessionSetProperty(as_session(session), key, Some(value)) };
    check_status(st, &format!("set properti {name}"))
}

/// Encoder H.264 hardware macOS (VideoToolbox).
pub struct MacosEncoder {
    session: CFRetained<VTCompressionSession>,
    config: EncoderConfig,
    next_pts: u64,
    /// Staging BGRA milik encoder; di-bungkus pixel buffer sekali.
    pixels: Vec<u8>,
    /// Pixel buffer reusable (dibuat lazy pada frame pertama).
    frame_buffer: Option<CFRetained<CVPixelBuffer>>,
}

impl FrameEncoder for MacosEncoder {
    fn new(config: EncoderConfig) -> Result<Self, EncodeError> {
        if config.width == 0 || config.height == 0 {
            return Err(EncodeError::Unsupported(
                "resolusi 0 tidak valid".to_string(),
            ));
        }
        if config.profile == H264Profile::Baseline
            && config.entropy_mode == Some(H264EntropyMode::Cabac)
        {
            return Err(EncodeError::Unsupported(
                "CABAC tidak kompatibel dengan H.264 Baseline".to_string(),
            ));
        }
        // SAFETY: membaca extern static Apple yang selalu valid.
        let profile_key: &CFString = unsafe {
            match config.profile {
                H264Profile::Main => kVTProfileLevel_H264_Main_AutoLevel,
                H264Profile::High => kVTProfileLevel_H264_High_AutoLevel,
                H264Profile::Baseline => kVTProfileLevel_H264_Baseline_AutoLevel,
            }
        };

        // SAFETY: pembuatan session VT standar; semua pointer out valid.
        let session: CFRetained<VTCompressionSession> = unsafe {
            let mut out: *mut VTCompressionSession = std::ptr::null_mut();
            let st = VTCompressionSession::create(
                None,
                config.width as i32,
                config.height as i32,
                kCMVideoCodecType_H264,
                None,
                None,
                None,
                Some(vt_output_callback),
                std::ptr::null_mut(),
                NonNull::new(&mut out).unwrap(),
            );
            check_status(st, "VTCompressionSessionCreate")?;
            if out.is_null() {
                return Err(EncodeError::Session("session null tanpa error".to_string()));
            }
            CFRetained::retain(NonNull::new(out).unwrap())
        };

        let avg_bitrate = CFNumber::new_i32(config.bitrate_bps as i32);
        let max_keyint = CFNumber::new_i32(config.keyframe_interval as i32);
        let fps_num = CFNumber::new_i32(config.fps as i32);
        // SAFETY: membaca extern static Apple yang selalu valid; nilai
        // properti bertipe CF yang benar untuk tiap key.
        unsafe {
            // Profile harus dipasang sebelum entropy mode; kombinasi yang
            // tidak kompatibel dapat menghasilkan bitstream noncompliant.
            set_property(
                &session,
                kVTCompressionPropertyKey_ProfileLevel,
                as_cftype(profile_key),
                "ProfileLevel",
            )?;
            if let Some(mode) = config.entropy_mode {
                let entropy = match mode {
                    H264EntropyMode::Cavlc => kVTH264EntropyMode_CAVLC,
                    H264EntropyMode::Cabac => kVTH264EntropyMode_CABAC,
                };
                set_property(
                    &session,
                    kVTCompressionPropertyKey_H264EntropyMode,
                    as_cftype(entropy),
                    "H264EntropyMode",
                )?;
            }
            match config.quality {
                // Constant quality (T6): teks/desktop lebih tajam, bandwidth
                // variabel. Batasi window 1 detik agar konten padat tidak
                // membuat bitrate melonjak tanpa batas. Bila Quality ditolak,
                // fallback ke target AverageBitRate seperti sebelumnya.
                Some(q) => {
                    let quality = CFNumber::new_f32(q.clamp(0.0, 1.0));
                    if let Err(e) = set_property(
                        &session,
                        kVTCompressionPropertyKey_Quality,
                        as_cftype(&*quality),
                        "Quality",
                    ) {
                        tracing::warn!("VT menolak Quality ({e}); fallback AverageBitRate");
                        set_property(
                            &session,
                            kVTCompressionPropertyKey_AverageBitRate,
                            as_cftype(&*avg_bitrate),
                            "AverageBitRate",
                        )?;
                    } else {
                        // DataRateLimits = [byte limit, duration seconds].
                        // Pembulatan ke atas mencegah cap lebih rendah dari
                        // bitrate_bps saat nilainya tidak habis dibagi 8.
                        let cap_bytes =
                            CFNumber::new_i64((config.bitrate_bps as u64).div_ceil(8) as i64);
                        let cap_window = CFNumber::new_f64(1.0);
                        let limits =
                            CFArray::<CFNumber>::from_objects(&[&*cap_bytes, &*cap_window]);
                        if let Err(e) = set_property(
                            &session,
                            kVTCompressionPropertyKey_DataRateLimits,
                            as_cftype(&*limits),
                            "DataRateLimits",
                        ) {
                            // Apple menyatakan tidak semua codec mendukung
                            // properti ini. Jangan mematikan streaming, tetapi
                            // jangan juga mengklaim cap aktif bila ditolak.
                            tracing::warn!(
                                bitrate_cap_bps = config.bitrate_bps,
                                "VT menolak DataRateLimits ({e}); quality mode berjalan tanpa hard cap"
                            );
                        }
                    }
                }
                None => {
                    set_property(
                        &session,
                        kVTCompressionPropertyKey_AverageBitRate,
                        as_cftype(&*avg_bitrate),
                        "AverageBitRate",
                    )?;
                }
            }
            set_property(
                &session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                as_cftype(&*max_keyint),
                "MaxKeyFrameInterval",
            )?;
            set_property(
                &session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                as_cftype(&*fps_num),
                "ExpectedFrameRate",
            )?;
            set_property(
                &session,
                kVTCompressionPropertyKey_RealTime,
                as_cftype(CFBoolean::new(true)),
                "RealTime",
            )?;
            set_property(
                &session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                as_cftype(CFBoolean::new(false)),
                "AllowFrameReordering",
            )?;
        }

        // SAFETY: session valid hasil create.
        let st = unsafe { session.prepare_to_encode_frames() };
        check_status(st, "PrepareToEncodeFrames")?;

        Ok(Self {
            session,
            config,
            next_pts: 0,
            pixels: vec![0u8; config.width as usize * config.height as usize * 4],
            frame_buffer: None,
        })
    }

    fn encode_frame(&mut self, frame: &Frame) -> Result<Vec<EncodedPacket>, EncodeError> {
        let w = self.config.width as usize;
        let h = self.config.height as usize;
        if frame.width as usize != w || frame.height as usize != h {
            return Err(EncodeError::Encode(format!(
                "ukuran frame {}x{} != encoder {}x{}",
                frame.width, frame.height, w, h
            )));
        }
        // SAFETY: pixel buffer reusable milik encoder; VT selesai dengan
        // frame SEBELUMNYA karena encode_frame menunggu callback output.
        let pix = unsafe {
            let pb = ensure_pixel_buffer(&mut self.pixels, &mut self.frame_buffer, w, h)?;
            copy_frame_into(&pb, frame)?;
            pb
        };
        // SAFETY: encode mengikuti kontrak VT; refcon Box diambil alih
        // callback tepat sekali.
        unsafe {
            let (tx, rx) = mpsc::channel::<EncodeOutcome>();
            let refcon = Box::into_raw(Box::new(tx)) as *mut c_void;
            let pts = CMTime::new(self.next_pts as i64, self.config.fps as i32);
            let dur = CMTime::new(1, self.config.fps as i32);
            let mut flags_out: VTEncodeInfoFlags = VTEncodeInfoFlags(0);
            let st = self.session.encode_frame(
                &pix,
                pts,
                dur,
                None,
                refcon,
                &mut flags_out as *mut VTEncodeInfoFlags,
            );
            if st != 0 {
                // EncodeFrame gagal sinkron: reklamasi Box refcon.
                drop(Box::from_raw(refcon as *mut Sender<EncodeOutcome>));
                return Err(EncodeError::Encode(format!(
                    "VTCompressionSessionEncodeFrame gagal (OSStatus {st})"
                )));
            }
            let my_pts = self.next_pts;
            self.next_pts += 1;
            // `pix` (clone) dilepas di sini; VT menyimpan ref sendiri sampai
            // callback, yang kita tunggu di bawah — jadi buffer aman dipakai
            // ulang setelah fungsi ini kembali.
            drop(pix);
            match rx.recv_timeout(ENCODE_TIMEOUT) {
                Ok(EncodeOutcome::Packets(payloads)) => Ok(payloads
                    .into_iter()
                    .map(|p| EncodedPacket {
                        data: p.annexb,
                        is_keyframe: p.is_keyframe,
                        pts: my_pts,
                    })
                    .collect()),
                Ok(EncodeOutcome::Failed(msg)) => Err(EncodeError::Encode(msg)),
                Err(_) => Err(EncodeError::Encode(format!(
                    "timeout menunggu hasil encode frame {my_pts}"
                ))),
            }
        }
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
        // SAFETY: session valid; kCMTimeInvalid => kuras semua pending.
        let st = unsafe { self.session.complete_frames(kCMTimeInvalid) };
        check_status(st, "CompleteFrames")?;
        Ok(Vec::new())
    }
}

impl Drop for MacosEncoder {
    fn drop(&mut self) {
        // SAFETY: session valid milik sendiri; invalidate idempoten.
        unsafe {
            self.session.invalidate();
        }
    }
}
