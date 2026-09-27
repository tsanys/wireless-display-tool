//! Encoder Opus 48 kHz stereo, frame 20 ms (960 sampel/kanal).
//!
//! Memakai `opus` crate (binding libopus yang di-vendor; `opusic-sys`
//! meng-compile libopus via cmake saat build lalu **static-link** — tidak
//! ada dependency native runtime tersembunyi). Trade-off didokumentasikan:
//! cmake adalah kebutuhan build-time, sama seperti toolchain C pada umumnya.
//!
//! Parameter default: application=Audio (musik/desktop), VBR on, bitrate
//! ~96 kbps, inband FEC off, DTX off (DTX bisa mengubah durasi paket dan
//! menyulitkan timestamp — dimatikan agar tiap paket konsisten 20 ms).

use super::{AudioCaptureError, OUT_SAMPLE_RATE, OUT_SAMPLES_PER_CHUNK};

/// Bitrate target Opus.
pub const DEFAULT_BITRATE_BPS: i32 = 96_000;

/// Ukuran maksimum satu paket Opus (batas aman).
const MAX_PACKET: usize = 4_000;

/// Encoder Opus stateful untuk satu track audio.
pub struct OpusStreamEncoder {
    inner: opus::Encoder,
    packets: u64,
    bytes: u64,
}

impl OpusStreamEncoder {
    /// Encoder 48 kHz stereo application=Audio dengan bitrate default.
    pub fn new() -> Result<Self, AudioCaptureError> {
        Self::with_bitrate(DEFAULT_BITRATE_BPS)
    }

    pub fn with_bitrate(bitrate_bps: i32) -> Result<Self, AudioCaptureError> {
        let mut inner = opus::Encoder::new(
            OUT_SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )
        .map_err(|e| AudioCaptureError::Session(format!("opus encoder: {e}")))?;
        // Best-effort: encoder tetap valid bila setelan opsional ditolak.
        let _ = inner.set_bitrate(opus::Bitrate::Bits(bitrate_bps));
        let _ = inner.set_vbr(true);
        let _ = inner.set_inband_fec(false);
        let _ = inner.set_dtx(false);
        Ok(Self {
            inner,
            packets: 0,
            bytes: 0,
        })
    }

    /// Encode tepat satu chunk 20 ms (960 frame × 2 kanal = 1920 sampel i16).
    pub fn encode_chunk(&mut self, chunk: &[i16]) -> Result<Vec<u8>, AudioCaptureError> {
        let expected = OUT_SAMPLES_PER_CHUNK * 2;
        if chunk.len() != expected {
            return Err(AudioCaptureError::Format(format!(
                "chunk Opus harus {expected} sampel, dapat {}",
                chunk.len()
            )));
        }
        let mut out = vec![0u8; MAX_PACKET];
        let n = self
            .inner
            .encode(chunk, &mut out)
            .map_err(|e| AudioCaptureError::Session(format!("opus encode: {e}")))?;
        out.truncate(n);
        self.packets += 1;
        self.bytes += n as u64;
        Ok(out)
    }

    pub fn packets_encoded(&self) -> u64 {
        self.packets
    }

    pub fn bytes_encoded(&self) -> u64 {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize, freq: f32) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let v =
                    (2.0 * std::f32::consts::PI * freq * i as f32 / OUT_SAMPLE_RATE as f32).sin();
                (v * 12_000.0) as i16
            })
            .collect()
    }

    #[test]
    fn encodes_exact_20ms_frame() {
        let mut enc = OpusStreamEncoder::new().expect("encoder");
        let chunk = tone(OUT_SAMPLES_PER_CHUNK * 2, 440.0);
        let pkt = enc.encode_chunk(&chunk).expect("encode");
        // Paket Opus 20 ms wajar berada di kisaran puluhan–ratusan byte.
        assert!(!pkt.is_empty(), "paket tidak boleh kosong");
        assert!(pkt.len() < 500, "paket 20 ms terlalu besar: {}", pkt.len());
        assert_eq!(enc.packets_encoded(), 1);
        assert_eq!(enc.bytes_encoded() as usize, pkt.len());
    }

    #[test]
    fn all_frames_have_constant_20ms_packet_duration() {
        // Setiap chunk encode harus menghasilkan satu paket (durasi 20 ms
        // diidentifikasi dari panjang sampel input, bukan dari isi paket).
        let mut enc = OpusStreamEncoder::new().expect("encoder");
        for _ in 0..50 {
            let chunk = tone(OUT_SAMPLES_PER_CHUNK * 2, 330.0);
            let pkt = enc.encode_chunk(&chunk).expect("encode");
            assert!(!pkt.is_empty());
        }
        assert_eq!(enc.packets_encoded(), 50);
    }

    #[test]
    fn rejects_wrong_chunk_size() {
        let mut enc = OpusStreamEncoder::new().expect("encoder");
        assert!(enc.encode_chunk(&vec![0i16; 100]).is_err());
    }

    #[test]
    fn silence_encodes_to_tiny_packet() {
        let mut enc = OpusStreamEncoder::new().expect("encoder");
        let pkt = enc
            .encode_chunk(&vec![0i16; OUT_SAMPLES_PER_CHUNK * 2])
            .expect("encode");
        assert!(pkt.len() < 16, "silence harus sangat kecil: {}", pkt.len());
    }
}
