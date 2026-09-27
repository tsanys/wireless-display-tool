//! Windows H.264 hardware encoding via Media Foundation.
//!
//! Enumerasi MFT hardware (`MFTEnumEx` + `MFT_ENUM_FLAG_HARDWARE`, input
//! NV12 → output H264) — vendor-agnostik (NVENC / Quick Sync / VCE).
//! Tanpa HW encoder → [`EncodeError::NoHardwareEncoder`], tanpa fallback
//! software. Input [`Frame`](crate::capture::Frame) BGRA dikonversi ke
//! NV12 (BT.709) karena itulah format yang diminta encoder HW (dipastikan
//! via Microsoft Learn). Output sample MFT H.264 adalah Annex B; SPS/PPS
//! dari `MF_MT_MPEG_SEQUENCE_HEADER` disisipkan sebelum tiap IDR.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Media::MediaFoundation::ICodecAPI;
use windows::Win32::Media::MediaFoundation::eAVEncCommonRateControlMode_CBR;
use windows::Win32::Media::MediaFoundation::eAVEncCommonRateControlMode_Quality;
use windows::Win32::Media::MediaFoundation::eAVEncH264VProfile_High;
use windows::Win32::Media::MediaFoundation::eAVEncH264VProfile_Main;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonQuality,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncH264CABACEnable,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, IMFActivate, IMFMediaBuffer,
    IMFSample, IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MFCreateAlignedMemoryBuffer,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_INFO, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_I4};
use windows::core::Interface;

use super::{
    EncodeError, EncodedPacket, EncoderConfig, FrameEncoder, H264EntropyMode, H264Profile,
};
use crate::capture::Frame;

/// NALU H.264: tipe 5 = IDR.
const NAL_TYPE_IDR: u8 = 5;

fn variant_i4(v: i32) -> VARIANT {
    let mut var = VARIANT::default();
    // SAFETY: menulis union VARIANT sesuai layout VT_I4 standar Win32.
    unsafe {
        let a = &mut *var.Anonymous.Anonymous;
        a.vt = VT_I4;
        a.Anonymous.lVal = v;
    }
    var
}

fn variant_bool(v: bool) -> VARIANT {
    let mut var = VARIANT::default();
    // SAFETY: menulis union VARIANT sesuai layout VT_BOOL standar Win32.
    unsafe {
        let a = &mut *var.Anonymous.Anonymous;
        a.vt = VT_BOOL;
        a.Anonymous.boolVal = if v { VARIANT_TRUE } else { VARIANT_FALSE };
    }
    var
}

fn mf_err(what: &str, e: windows::core::Error) -> EncodeError {
    EncodeError::Session(format!("{what} gagal: {e}"))
}

/// Konversi BGRA8 (stride bebas) → NV12 BT.709 ke buffer `dst` yang
/// **dipakai ulang** (resize hanya bila ukuran berubah).
/// Encoder MFT menuntut dimensi genap (chroma subsampling 4:2:0).
fn bgra_to_nv12_into(dst: &mut Vec<u8>, frame: &Frame) -> Result<(u32, u32), EncodeError> {
    let w = frame.width & !1; // genapkan
    let h = frame.height & !1;
    if w == 0 || h == 0 {
        return Err(EncodeError::Unsupported(
            "dimensi frame < 2 piksel setelah dibulatkan ke genap".to_string(),
        ));
    }
    let src_stride = frame.stride as usize;
    let y_plane = (w as usize) * (h as usize);
    let uv_plane = (w as usize) * (h as usize / 2);
    let total = y_plane + uv_plane;
    if dst.len() != total {
        dst.resize(total, 0);
    }
    let nv12: &mut [u8] = dst;

    for row in 0..h as usize {
        let src = frame
            .data
            .get(row * src_stride..row * src_stride + w as usize * 4)
            .ok_or_else(|| EncodeError::Encode("baris BGRA terpotong".to_string()))?;
        let y_dst = &mut nv12[row * w as usize..(row + 1) * w as usize];
        for (x, px) in src.chunks_exact(4).enumerate() {
            let (b, g, r) = (px[0] as f32, px[1] as f32, px[2] as f32);
            y_dst[x] = (0.2126 * r + 0.7152 * g + 0.0722 * b + 0.5) as u8;
        }
    }
    // UV interleave (V lalu U per 2x2 blok) — BT.709 limited range chroma.
    for cy in 0..h as usize / 2 {
        for cx in 0..w as usize / 2 {
            let mut cb_acc = [0f32; 4];
            let mut cr_acc = [0f32; 4];
            for (dy, dx) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)] {
                let row = cy * 2 + dy;
                let off = row * src_stride + (cx * 2 + dx) * 4;
                let px = frame
                    .data
                    .get(off..off + 4)
                    .ok_or_else(|| EncodeError::Encode("piksel BGRA terpotong".to_string()))?;
                let (b, g, r) = (px[0] as f32, px[1] as f32, px[2] as f32);
                cb_acc[dy * 2 + dx] = -0.114572 * r - 0.385428 * g + 0.5 * b + 128.0;
                cr_acc[dy * 2 + dx] = 0.5 * r - 0.454153 * g - 0.045847 * b + 128.0;
            }
            let uv_off = y_plane + (cy * w as usize + cx * 2);
            nv12[uv_off] = (cb_acc.iter().sum::<f32>() / 4.0 + 0.5) as u8; // U
            nv12[uv_off + 1] = (cr_acc.iter().sum::<f32>() / 4.0 + 0.5) as u8; // V
        }
    }
    Ok((w, h))
}

/// Deteksi apakah bitstream Annex B memuat NALU IDR.
fn contains_idr(annexb: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 4 <= annexb.len() {
        // cari start code 00 00 01 / 00 00 00 01
        if annexb[i..].starts_with(&[0, 0, 1]) {
            let sc = if i >= 1 && annexb[i - 1] == 0 { 4 } else { 3 };
            let _ = sc;
            let hdr = i + 3;
            if hdr < annexb.len() {
                let nal_type = annexb[hdr] & 0x1f;
                if nal_type == NAL_TYPE_IDR {
                    return true;
                }
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Encoder H.264 hardware Windows (Media Foundation MFT).
pub struct WindowsEncoder {
    config: EncoderConfig,
    activate: IMFActivate,
    transform: IMFTransform,
    codec_api: ICodecAPI,
    /// SPS+PPS Annex B dari MF_MT_MPEG_SEQUENCE_HEADER (dari output type).
    sps_pps: Vec<u8>,
    /// Dimensi NV12 aktual (genap).
    enc_width: u32,
    enc_height: u32,
    next_pts: u64,
    frame_index: u64,
    /// Sample output reuse (ukuran buffer keluaran sudah dialokasikan).
    out_sample: IMFSample,
    /// Staging NV12 milik encoder (dipakai ulang tiap frame).
    nv12: Vec<u8>,
    /// Sample input reuse (menghindari MFCreateSample/MemoryBuffer per frame).
    in_sample: IMFSample,
    in_buffer: IMFMediaBuffer,
}

impl FrameEncoder for WindowsEncoder {
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
        // MFT encoder banyak yang menuntut dimensi genap; genapkan di sini
        // supaya capture tetap bisa memakai dimensi apa pun.
        let enc_width = config.width & !1;
        let enc_height = config.height & !1;
        if enc_width == 0 || enc_height == 0 {
            return Err(EncodeError::Unsupported(
                "dimensi < 2 piksel setelah dibulatkan ke genap".to_string(),
            ));
        }

        // SAFETY: seluruh blok mengikuti kontrak MF yang terverifikasi di
        // Microsoft Learn (output type sebelum input type; ICodecAPI sebelum
        // streaming). COM di-init MTA per-thread.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| mf_err("CoInitializeEx", e))?;

            // Enumerasi HW encoder pertama (vendor-agnostik).
            let input = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_NV12,
            };
            let output = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut activates,
                &mut count,
            )
            .map_err(|e| mf_err("MFTEnumEx", e))?;
            if count == 0 {
                return Err(EncodeError::NoHardwareEncoder(
                    "tidak ada MFT hardware H.264 (NV12→H264) di mesin ini".to_string(),
                ));
            }
            let activate: IMFActivate = (*activates)
                .clone()
                .ok_or_else(|| EncodeError::NoHardwareEncoder("IMFActivate null".to_string()))?;
            // Array activates dikembalikan CoTaskMemAlloc-style; bebaskan
            // slot sisanya lalu array-nya.
            for i in 1..count as usize {
                std::mem::drop((*activates.add(i)).take());
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));

            let transform: IMFTransform = activate
                .ActivateObject()
                .map_err(|e| mf_err("ActivateObject", e))?;
            let codec_api: ICodecAPI = transform.cast().map_err(|e| mf_err("cast ICodecAPI", e))?;

            // Output media type (SEBELUM input) sesuai Microsoft Learn.
            let out_type = MFCreateMediaType().map_err(|e| mf_err("MFCreateMediaType", e))?;
            out_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| mf_err("SetGUID major", e))?;
            out_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|e| mf_err("SetGUID subtype", e))?;
            out_type
                .SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (enc_height as u64) << 32 | enc_width as u64,
                )
                .map_err(|e| mf_err("SetUINT64 frame size", e))?;
            out_type
                .SetUINT64(&MF_MT_FRAME_RATE, (config.fps as u64) << 32 | 1)
                .map_err(|e| mf_err("SetUINT64 frame rate", e))?;
            out_type
                .SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate_bps)
                .map_err(|e| mf_err("SetUINT32 bitrate", e))?;
            out_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| mf_err("SetUINT32 interlace", e))?;
            let profile = match config.profile {
                H264Profile::Main => eAVEncH264VProfile_Main,
                H264Profile::High => eAVEncH264VProfile_High,
                H264Profile::Baseline => {
                    windows::Win32::Media::MediaFoundation::eAVEncH264VProfile_Base
                }
            };
            out_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, profile.0 as u32)
                .map_err(|e| mf_err("SetUINT32 profile", e))?;
            transform
                .SetOutputType(0, &out_type, 0)
                .map_err(|e| mf_err("SetOutputType", e))?;

            // ICodecAPI: CBR + bitrate + GOP + tanpa B-frame + low latency.
            // Properti rate-control wajib di-set sebelum streaming.
            let set_i4 =
                |api: &windows::core::GUID, v: i32, name: &str| -> Result<(), EncodeError> {
                    codec_api
                        .SetValue(api, &variant_i4(v))
                        .map_err(|e| mf_err(&format!("ICodecAPI {name}"), e))
                };
            match config.quality {
                // Constant quality (T6): teks/desktop lebih tajam. Best-effort
                // (MFT tertentu mengabaikan); properti low-latency tetap wajib.
                Some(q) => {
                    let q = (q.clamp(0.0, 1.0) * 100.0).round() as i32;
                    let _ = set_i4(
                        &CODECAPI_AVEncCommonRateControlMode,
                        eAVEncCommonRateControlMode_Quality.0,
                        "RateControlMode=Quality",
                    );
                    let _ = set_i4(&CODECAPI_AVEncCommonQuality, q, "CommonQuality");
                    // Dokumentasi Microsoft hanya menjamin MaxBitRate untuk
                    // PeakConstrainedVBR, bukan Quality. Sejumlah MFT vendor
                    // tetap menerimanya di quality mode, jadi terapkan sebagai
                    // cap best-effort dan laporkan bila ditolak.
                    if let Err(e) = set_i4(
                        &CODECAPI_AVEncCommonMaxBitRate,
                        config.bitrate_bps as i32,
                        "MaxBitRate (quality best-effort)",
                    ) {
                        tracing::warn!(
                            bitrate_cap_bps = config.bitrate_bps,
                            "MFT menolak MaxBitRate di quality mode ({e}); encoder berjalan tanpa cap terjamin"
                        );
                    }
                }
                None => {
                    set_i4(
                        &CODECAPI_AVEncCommonRateControlMode,
                        eAVEncCommonRateControlMode_CBR.0,
                        "RateControlMode=CBR",
                    )?;
                    set_i4(
                        &CODECAPI_AVEncCommonMeanBitRate,
                        config.bitrate_bps as i32,
                        "MeanBitRate",
                    )?;
                }
            }
            if let Some(mode) = config.entropy_mode {
                codec_api
                    .SetValue(
                        &CODECAPI_AVEncH264CABACEnable,
                        &variant_bool(mode == H264EntropyMode::Cabac),
                    )
                    .map_err(|e| mf_err("ICodecAPI H264CABACEnable", e))?;
            }
            set_i4(
                &CODECAPI_AVEncMPVGOPSize,
                config.keyframe_interval as i32,
                "GOPSize",
            )?;
            set_i4(&CODECAPI_AVEncMPVDefaultBPictureCount, 0, "BPictureCount=0")?;
            set_i4(&CODECAPI_AVLowLatencyMode, 1, "LowLatency")?;

            // Input media type NV12.
            let in_type = MFCreateMediaType().map_err(|e| mf_err("MFCreateMediaType", e))?;
            in_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| mf_err("SetGUID major", e))?;
            in_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(|e| mf_err("SetGUID subtype", e))?;
            in_type
                .SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (enc_height as u64) << 32 | enc_width as u64,
                )
                .map_err(|e| mf_err("SetUINT64 frame size", e))?;
            transform
                .SetInputType(0, &in_type, 0)
                .map_err(|e| mf_err("SetInputType", e))?;

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| mf_err("BEGIN_STREAMING", e))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| mf_err("START_OF_STREAM", e))?;

            // SPS/PPS dari output type (blob berisi SPS+PPS Annex B).
            let mut sps_pps = Vec::new();
            if let Ok(size) = out_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                if size > 0 {
                    let mut buf = vec![0u8; size as usize];
                    if out_type
                        .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buf, None)
                        .is_ok()
                    {
                        sps_pps = buf;
                    }
                }
            }

            // Sample output reuse sesuai GetOutputStreamInfo.
            let info: MFT_OUTPUT_STREAM_INFO = transform
                .GetOutputStreamInfo(0)
                .map_err(|e| mf_err("GetOutputStreamInfo", e))?;
            let capacity = if info.cbSize == 0 {
                // Perkiraan aman: bitrate * 1 dtk kalau MFT PROVIDES_SAMPLES.
                config.bitrate_bps / 8
            } else {
                info.cbSize
            };
            let out_sample = make_output_sample(capacity, info.cbAlignment)?;

            // Sample input + buffer reuse (ukuran NV12 tetap). MFT sinkron:
            // sample input boleh dipakai ulang setelah ProcessInput kembali.
            let nv12_len = (enc_width as usize) * (enc_height as usize) * 3 / 2;
            let in_buffer = MFCreateMemoryBuffer(nv12_len as u32)
                .map_err(|e| mf_err("MFCreateMemoryBuffer(input)", e))?;
            let in_sample = MFCreateSample().map_err(|e| mf_err("MFCreateSample(input)", e))?;
            in_sample
                .AddBuffer(&in_buffer)
                .map_err(|e| mf_err("AddBuffer(input)", e))?;

            Ok(Self {
                config,
                activate,
                transform,
                codec_api,
                sps_pps,
                enc_width,
                enc_height,
                next_pts: 0,
                frame_index: 0,
                out_sample,
                nv12: Vec::new(),
                in_sample,
                in_buffer,
            })
        }
    }

    fn encode_frame(&mut self, frame: &Frame) -> Result<Vec<EncodedPacket>, EncodeError> {
        // Isi ulang buffer NV12 milik encoder (tanpa alokasi per frame).
        let (w, h) = bgra_to_nv12_into(&mut self.nv12, frame)?;
        if w != self.enc_width || h != self.enc_height {
            return Err(EncodeError::Unsupported(format!(
                "frame {}x{} tidak cocok dengan session {}x{}",
                w, h, self.enc_width, self.enc_height
            )));
        }
        let nv12_len = self.nv12.len();
        let pts = self.next_pts;
        self.next_pts += 1;

        // SAFETY: kontrak ProcessInput/ProcessOutput MFT sinkron standar.
        unsafe {
            // Paksa keyframe tiap keyframe_interval (frame 0 juga).
            if self.frame_index % self.config.keyframe_interval as u64 == 0 {
                self.codec_api
                    .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_i4(1))
                    .map_err(|e| mf_err("ForceKeyFrame", e))?;
            }
            self.frame_index += 1;

            // Sample input dipakai ulang: lock → salin → unlock → set panjang.
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            self.in_buffer
                .Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
                .map_err(|e| mf_err("Lock(input)", e))?;
            std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), ptr, nv12_len);
            self.in_buffer
                .Unlock()
                .map_err(|e| mf_err("Unlock(input)", e))?;
            self.in_buffer
                .SetCurrentLength(nv12_len as u32)
                .map_err(|e| mf_err("SetCurrentLength(input)", e))?;
            self.in_sample
                .SetSampleTime((pts * 10_000_000 / self.config.fps as u64) as i64)
                .map_err(|e| mf_err("SetSampleTime", e))?;
            self.in_sample
                .SetSampleDuration(10_000_000 / self.config.fps as i64)
                .map_err(|e| mf_err("SetSampleDuration", e))?;
            self.transform
                .ProcessInput(0, &self.in_sample, 0)
                .map_err(|e| mf_err("ProcessInput", e))?;

            // Drain output yang siap (low-latency: 1 input → 0..n output).
            let mut packets = Vec::new();
            loop {
                let mft_out = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(self.out_sample.clone())),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut [mft_out], &mut status);
                match result {
                    Ok(()) => {
                        let data = read_sample(&self.out_sample)?;
                        if !data.is_empty() {
                            let is_idr = contains_idr(&data);
                            let final_data = if is_idr && !self.sps_pps.is_empty() {
                                let mut d = self.sps_pps.clone();
                                d.extend_from_slice(&data);
                                d
                            } else {
                                data
                            };
                            packets.push(EncodedPacket {
                                data: final_data,
                                is_keyframe: is_idr,
                                pts,
                            });
                        }
                        reset_sample(&self.out_sample)?;
                    }
                    Err(e) => {
                        let code = e.code();
                        if code == MF_E_TRANSFORM_NEED_MORE_INPUT {
                            break; // normal: encoder menahan frame
                        }
                        if code == MF_E_TRANSFORM_STREAM_CHANGE {
                            // Renegosiasi tipe output: ambil ulang tipe pertama.
                            renegotiate(&self.transform)?;
                            continue;
                        }
                        return Err(mf_err("ProcessOutput", e));
                    }
                }
            }
            Ok(packets)
        }
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
        // SAFETY: DRAIN lalu kosongkan output tersisa.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(|e| mf_err("DRAIN", e))?;
            let mut packets = Vec::new();
            loop {
                let mft_out = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(self.out_sample.clone())),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut [mft_out], &mut status);
                match result {
                    Ok(()) => {
                        let data = read_sample(&self.out_sample)?;
                        if data.is_empty() {
                            break;
                        }
                        let is_idr = contains_idr(&data);
                        let final_data = if is_idr && !self.sps_pps.is_empty() {
                            let mut d = self.sps_pps.clone();
                            d.extend_from_slice(&data);
                            d
                        } else {
                            data
                        };
                        packets.push(EncodedPacket {
                            data: final_data,
                            is_keyframe: is_idr,
                            pts: self.next_pts,
                        });
                        reset_sample(&self.out_sample)?;
                    }
                    Err(e) => {
                        let code = e.code();
                        if code == MF_E_TRANSFORM_NEED_MORE_INPUT
                            || code == MF_E_TRANSFORM_STREAM_CHANGE
                        {
                            break;
                        }
                        return Err(mf_err("ProcessOutput(flush)", e));
                    }
                }
            }
            Ok(packets)
        }
    }
}

impl Drop for WindowsEncoder {
    fn drop(&mut self) {
        // SAFETY: teardown MFT sesuai urutan yang didokumentasikan.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.activate.ShutdownObject();
        }
    }
}

fn make_output_sample(capacity: u32, alignment: u32) -> Result<IMFSample, EncodeError> {
    // SAFETY: pembuatan sample + buffer MF standar.
    unsafe {
        let buf = if alignment > 1 {
            MFCreateAlignedMemoryBuffer(capacity, alignment)
        } else {
            MFCreateMemoryBuffer(capacity)
        }
        .map_err(|e| mf_err("MFCreate*Buffer(output)", e))?;
        let sample = MFCreateSample().map_err(|e| mf_err("MFCreateSample", e))?;
        sample
            .AddBuffer(&buf)
            .map_err(|e| mf_err("AddBuffer(output)", e))?;
        Ok(sample)
    }
}

/// Baca bytes dari seluruh buffer sample output.
fn read_sample(sample: &IMFSample) -> Result<Vec<u8>, EncodeError> {
    // SAFETY: Lock/Unlock berpasangan; panjang dibaca dulu.
    unsafe {
        let mut total = Vec::new();
        let count = sample
            .GetBufferCount()
            .map_err(|e| mf_err("GetBufferCount", e))?;
        for i in 0..count {
            let buf: IMFMediaBuffer = sample
                .GetBufferByIndex(i)
                .map_err(|e| mf_err("GetBufferByIndex", e))?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
                .map_err(|e| mf_err("Lock(output)", e))?;
            let slice = std::slice::from_raw_parts(ptr, cur_len as usize);
            total.extend_from_slice(slice);
            buf.Unlock().map_err(|e| mf_err("Unlock(output)", e))?;
        }
        Ok(total)
    }
}

/// Kosongkan sample output untuk dipakai ulang.
fn reset_sample(sample: &IMFSample) -> Result<(), EncodeError> {
    // SAFETY: iterasi buffer dan SetCurrentLength(0).
    unsafe {
        let count = sample
            .GetBufferCount()
            .map_err(|e| mf_err("GetBufferCount", e))?;
        for i in 0..count {
            let buf: IMFMediaBuffer = sample
                .GetBufferByIndex(i)
                .map_err(|e| mf_err("GetBufferByIndex", e))?;
            buf.SetCurrentLength(0)
                .map_err(|e| mf_err("SetCurrentLength", e))?;
        }
        Ok(())
    }
}

/// Tangani MF_E_TRANSFORM_STREAM_CHANGE: SetOutputType ulang dari tipe
/// pertama yang tersedia.
fn renegotiate(transform: &IMFTransform) -> Result<(), EncodeError> {
    // SAFETY: GetOutputAvailableType index 0 lalu SetOutputType.
    unsafe {
        let mt = transform
            .GetOutputAvailableType(0, 0)
            .map_err(|e| mf_err("GetOutputAvailableType", e))?;
        transform
            .SetOutputType(0, &mt, 0)
            .map_err(|e| mf_err("SetOutputType(renegotiate)", e))?;
        Ok(())
    }
}
