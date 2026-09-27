//! Normalisasi PCM: konversi format mentah capturer → f32 interleaved
//! stereo, dan rechunking ke chunk tetap 960 frame (20 ms @ 48 kHz) i16
//! untuk encoder Opus.

use super::{AudioCaptureError, OUT_SAMPLES_PER_CHUNK};

/// Format mentah PCM sumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmSourceFormat {
    /// Float32 interleaved (`frame = [L, R, L, R, …]`).
    F32Interleaved,
    /// Float32 planar/de-interleaved (satu buffer per kanal berurutan —
    /// format standar `AVAudioFormat(standardFormatWithSampleRate:)` yang
    /// dipakai ScreenCaptureKit).
    F32Planar,
    /// i16 interleaved.
    I16Interleaved,
}

/// Konversi bytes PCM mentah → f32 interleaved **stereo** (alokasi baru).
/// Lihat [`convert_to_stereo_f32_into`] untuk versi reuse buffer.
///
/// - mono → diduplikasi ke kedua kanal;
/// - >2 kanal → diambil 2 kanal pertama.
///
/// `data.len()` harus kelipatan ukuran frame format (diperiksa).
pub fn convert_to_stereo_f32(
    data: &[u8],
    format: PcmSourceFormat,
    channels: u16,
) -> Result<Vec<f32>, AudioCaptureError> {
    let mut out = Vec::new();
    convert_to_stereo_f32_into(&mut out, data, format, channels)?;
    Ok(out)
}

/// Seperti [`convert_to_stereo_f32`] tetapi menulis ke `dst` sehingga
/// kapasitasnya dipakai ulang antar panggilan (tanpa alokasi per-buffer bila
/// kapasitas cukup). `dst` dikosongkan lalu diisi ulang.
pub fn convert_to_stereo_f32_into(
    dst: &mut Vec<f32>,
    data: &[u8],
    format: PcmSourceFormat,
    channels: u16,
) -> Result<(), AudioCaptureError> {
    if channels == 0 {
        return Err(AudioCaptureError::Format("channel 0".into()));
    }
    let ch = channels as usize;
    let (bytes_per_sample, planar): (usize, bool) = match format {
        PcmSourceFormat::F32Interleaved => (4, false),
        PcmSourceFormat::F32Planar => (4, true),
        PcmSourceFormat::I16Interleaved => (2, false),
    };
    let frame_bytes = bytes_per_sample * ch;
    if frame_bytes == 0 || data.len() % frame_bytes != 0 {
        return Err(AudioCaptureError::Format(format!(
            "panjang buffer {} bukan kelipatan frame ({} byte)",
            data.len(),
            frame_bytes
        )));
    }
    let frames = data.len() / frame_bytes;
    dst.clear();
    dst.reserve(frames * 2);
    let read = |frame: usize, c: usize| -> f32 {
        let idx = if planar {
            c * frames + frame
        } else {
            frame * ch + c
        };
        let off = idx * bytes_per_sample;
        match format {
            PcmSourceFormat::F32Interleaved | PcmSourceFormat::F32Planar => {
                f32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            }
            PcmSourceFormat::I16Interleaved => {
                i16::from_le_bytes([data[off], data[off + 1]]) as f32 / 32768.0
            }
        }
    };
    for f in 0..frames {
        dst.push(read(f, 0));
        dst.push(read(f, 1 % ch));
    }
    Ok(())
}

/// Rechunker streaming: akumulasi f32 interleaved stereo → chunk i16
/// interleaved dengan jumlah frame tetap [`OUT_SAMPLES_PER_CHUNK`].
///
/// Statistik `chunks_out * 20 ms` adalah identitas durasi yang dipakai
/// sample WebRTC — inilah jaminan timestamp monotonik.
#[derive(Debug)]
pub struct OpusFrameChunker {
    staged: Vec<f32>,
    /// Total chunk (20 ms) yang sudah dikeluarkan — untuk drift-check.
    chunks_out: u64,
}

impl Default for OpusFrameChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl OpusFrameChunker {
    pub fn new() -> Self {
        Self {
            staged: Vec::with_capacity(OUT_SAMPLES_PER_CHUNK * 4),
            chunks_out: 0,
        }
    }

    /// Dorong sampel f32 interleaved stereo; kembalikan chunk penuh
    /// (0..n, masing-masing tepat `OUT_SAMPLES_PER_CHUNK * 2` sampel i16).
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<i16>> {
        self.staged.extend_from_slice(samples);
        let need = OUT_SAMPLES_PER_CHUNK * 2;
        let mut out = Vec::new();
        while self.staged.len() >= need {
            let chunk: Vec<i16> = self
                .staged
                .drain(..need)
                .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            self.chunks_out += 1;
            out.push(chunk);
        }
        out
    }

    /// Durasi media yang sudah dikeluarkan (identitas chunk × 20 ms).
    pub fn produced_duration(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.chunks_out * 20)
    }

    /// Sisa sampel yang belum membentuk chunk penuh.
    pub fn pending_samples(&self) -> usize {
        self.staged.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_f32_planar_to_interleaved_stereo() {
        // Planar: L = [1.0, 0.5], R = [-1.0, 0.0]
        let data: Vec<u8> = [1.0f32, 0.5, -1.0, 0.0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = convert_to_stereo_f32(&data, PcmSourceFormat::F32Planar, 2).unwrap();
        assert_eq!(out, vec![1.0, -1.0, 0.5, 0.0]);
    }

    #[test]
    fn converts_f32_interleaved_passthrough_shape() {
        let data: Vec<u8> = [0.25f32, -0.25, 0.5, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = convert_to_stereo_f32(&data, PcmSourceFormat::F32Interleaved, 2).unwrap();
        assert_eq!(out, vec![0.25, -0.25, 0.5, -0.5]);
    }

    #[test]
    fn duplicates_mono_to_stereo() {
        let data: Vec<u8> = [0.5f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = convert_to_stereo_f32(&data, PcmSourceFormat::F32Interleaved, 1).unwrap();
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn takes_first_two_of_multichannel() {
        let data: Vec<u8> = [1.0f32, 0.1, 0.2, 0.3, 0.9, 0.05, 0.15, 0.25]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = convert_to_stereo_f32(&data, PcmSourceFormat::F32Interleaved, 4).unwrap();
        assert_eq!(out, vec![1.0, 0.1, 0.9, 0.05]);
    }

    #[test]
    fn converts_i16_interleaved() {
        let data: Vec<u8> = [16384i16, -16384, 0, 32767]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = convert_to_stereo_f32(&data, PcmSourceFormat::I16Interleaved, 2).unwrap();
        assert!((out[0] - 0.5).abs() < 1e-3);
        assert!((out[1] + 0.5).abs() < 1e-3);
        assert_eq!(out[2], 0.0);
        assert!((out[3] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn rejects_truncated_pcm() {
        assert!(convert_to_stereo_f32(&[0, 0, 0], PcmSourceFormat::F32Interleaved, 2).is_err());
        assert!(convert_to_stereo_f32(&[1], PcmSourceFormat::I16Interleaved, 2).is_err());
    }

    #[test]
    fn convert_into_reuses_capacity_and_matches_allocating() {
        let data: Vec<u8> = [0.25f32, -0.25, 0.5, -0.5, 1.0, -1.0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut buf: Vec<f32> = Vec::new();
        convert_to_stereo_f32_into(&mut buf, &data, PcmSourceFormat::F32Interleaved, 2).unwrap();
        let expected = convert_to_stereo_f32(&data, PcmSourceFormat::F32Interleaved, 2).unwrap();
        assert_eq!(buf, expected);
        let cap = buf.capacity();
        // Panggilan kedua dengan data lebih pendek → isi diganti, kapasitas
        // tidak menyusut (dipakai ulang, tanpa alokasi baru).
        convert_to_stereo_f32_into(&mut buf, &data[..8], PcmSourceFormat::F32Interleaved, 2)
            .unwrap();
        assert_eq!(buf, vec![0.25, -0.25]);
        assert!(buf.capacity() >= cap);
    }

    #[test]
    fn convert_into_matches_planar_and_i16() {
        // Planar: L=[1.0,0.5], R=[-1.0,0.0].
        let planar: Vec<u8> = [1.0f32, 0.5, -1.0, 0.0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut buf = Vec::new();
        convert_to_stereo_f32_into(&mut buf, &planar, PcmSourceFormat::F32Planar, 2).unwrap();
        assert_eq!(buf, vec![1.0, -1.0, 0.5, 0.0]);

        let i16data: Vec<u8> = [16384i16, -16384]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        convert_to_stereo_f32_into(&mut buf, &i16data, PcmSourceFormat::I16Interleaved, 2).unwrap();
        assert!((buf[0] - 0.5).abs() < 1e-3);
        assert!((buf[1] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn chunker_emits_fixed_960_frame_chunks() {
        let mut c = OpusFrameChunker::new();
        // 2400 frame stereo = 2,5 chunk
        let samples: Vec<f32> = vec![0.1; 2400 * 2];
        let chunks = c.push(&samples);
        assert_eq!(chunks.len(), 2);
        for chunk in &chunks {
            assert_eq!(chunk.len(), OUT_SAMPLES_PER_CHUNK * 2);
        }
        // Sisa 480 frame (960 sampel) belum membentuk chunk.
        assert_eq!(c.pending_samples(), 480 * 2);
        assert_eq!(c.produced_duration(), std::time::Duration::from_millis(40));

        // Tambah tepat 480 frame → 1 chunk lagi, habis.
        let more = c.push(&vec![0.0; 480 * 2]);
        assert_eq!(more.len(), 1);
        assert_eq!(c.pending_samples(), 0);
        assert_eq!(c.produced_duration(), std::time::Duration::from_millis(60));
    }

    #[test]
    fn chunker_clamps_out_of_range_floats() {
        let mut c = OpusFrameChunker::new();
        let mut clipped = vec![2.0f32; OUT_SAMPLES_PER_CHUNK * 2];
        for (i, s) in clipped.iter_mut().enumerate() {
            *s = if i % 2 == 0 { 2.0 } else { -2.0 };
        }
        let chunks = c.push(&clipped);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0][0], 32767);
        assert_eq!(chunks[0][1], -32767);
    }
}
