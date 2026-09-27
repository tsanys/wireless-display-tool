//! Resampler streaming 48 kHz (pure-Rust, `rubato`) + mode passthrough.
//!
//! macOS: ScreenCaptureKit sudah menyampaikan audio pada rate yang kita
//! minta (48 kHz) → passthrough tanpa latensi tambahan.
//! Windows: mix format WASAPI bisa 44,1 kHz → resample FFT sinkron dengan
//! output tetap 960 frame per blok (fixed-output), staging input variabel.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use super::{AudioCaptureError, OUT_SAMPLE_RATE};

/// Resampler streaming: interleaved stereo f32 @ `in_rate` → interleaved
/// stereo f32 @ 48 kHz.
pub struct StreamResampler {
    in_rate: u32,
    passthrough: bool,
    inner: Option<Fft<f32>>,
    /// Staging input (interleaved stereo).
    staged: Vec<f32>,
    /// Buffer output sementara (ukuran tetap per blok).
    out_scratch: Vec<f32>,
}

impl StreamResampler {
    /// `in_rate == 48_000` → passthrough (tanpa rubato, tanpa delay).
    pub fn new(in_rate: u32, channels: u16) -> Result<Self, AudioCaptureError> {
        if in_rate == 0 || channels == 0 {
            return Err(AudioCaptureError::Format("rate/channel 0".into()));
        }
        if channels != 2 {
            return Err(AudioCaptureError::Format(format!(
                "resampler mendukung stereo, dapat {channels} kanal \
                 (normalisasi ke stereo harus terjadi duluan)"
            )));
        }
        if in_rate == OUT_SAMPLE_RATE {
            return Ok(Self {
                in_rate,
                passthrough: true,
                inner: None,
                staged: Vec::new(),
                out_scratch: Vec::new(),
            });
        }
        // Fixed-output: tiap process() menghasilkan tepat 960 frame dan
        // mengonsumsi `input_frames_next()` frame (bervariasi).
        let inner = Fft::new(
            in_rate as usize,
            OUT_SAMPLE_RATE as usize,
            super::OUT_SAMPLES_PER_CHUNK,
            2,
            FixedSync::Output,
        )
        .map_err(|e| AudioCaptureError::Session(format!("resampler: {e}")))?;
        Ok(Self {
            in_rate,
            passthrough: false,
            inner: Some(inner),
            staged: Vec::new(),
            out_scratch: vec![0.0; super::OUT_SAMPLES_PER_CHUNK * 2],
        })
    }

    pub fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    pub fn input_rate(&self) -> u32 {
        self.in_rate
    }

    /// Dorong sampel interleaved stereo; kembalikan output (bisa kosong,
    /// bisa beberapa blok 960 frame).
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>, AudioCaptureError> {
        if self.passthrough {
            return Ok(samples.to_vec());
        }
        self.staged.extend_from_slice(samples);
        let mut out = Vec::new();
        // Destructuring field: borrow `inner` (mut) + `staged`/`out_scratch`
        // (mut) secara disjoint agar lolos borrow checker.
        let Self {
            inner,
            staged,
            out_scratch,
            ..
        } = self;
        let Some(inner) = inner.as_mut() else {
            return Ok(out);
        };
        loop {
            let need = inner.input_frames_next();
            if staged.len() < need * 2 {
                break;
            }
            let input = InterleavedSlice::new(&staged[..need * 2], 2, need)
                .map_err(|e| AudioCaptureError::Session(format!("resampler in: {e}")))?;
            let out_frames = inner.output_frames_next();
            let out_len = out_frames * 2;
            if out_scratch.len() != out_len {
                out_scratch.resize(out_len, 0.0);
            }
            let mut output = InterleavedSlice::new_mut(&mut out_scratch[..out_len], 2, out_frames)
                .map_err(|e| AudioCaptureError::Session(format!("resampler out: {e}")))?;
            let (_, produced) = inner
                .process_into_buffer(&input, &mut output, None)
                .map_err(|e| AudioCaptureError::Session(format!("resampler proses: {e}")))?;
            let produced = produced.min(out_frames);
            out.extend_from_slice(&out_scratch[..produced * 2]);
            staged.drain(..need * 2);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sine 440 Hz dengan durasi tertentu pada rate tertentu.
    fn sine(rate: u32, ms: usize) -> Vec<f32> {
        let frames = rate as usize * ms / 1000;
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn passthrough_when_rate_matches() {
        let mut r = StreamResampler::new(48_000, 2).unwrap();
        assert!(r.is_passthrough());
        let in_ = sine(48_000, 20);
        let out = r.push(&in_).unwrap();
        assert_eq!(out.len(), in_.len());
        assert_eq!(out, in_);
    }

    #[test]
    fn resamples_44100_to_48000_with_exact_chunk_count() {
        let mut r = StreamResampler::new(44_100, 2).unwrap();
        assert!(!r.is_passthrough());
        // 1 detik input → total output harus mendekati 48000 frame
        // (48000 ± 1 blok karena delay/startup FFT).
        let input = sine(44_100, 1000);
        let mut produced: Vec<f32> = Vec::new();
        // Feed dalam chunk 512 frame (mensimulasikan paket WASAPI).
        for part in input.chunks(512 * 2) {
            produced.extend_from_slice(&r.push(part).unwrap());
        }
        let total_frames = produced.len() / 2;
        // FFT resampler memperkenalkan delay ~ setengah blok FFT; toleransi
        // 2 blok (40 ms) cukup longgar namun membuktikan rasio benar.
        assert!(
            (total_frames as i64 - 48_000).abs() < 2 * 960,
            "output {total_frames} frame, harapan ~48000"
        );

        // Energi tidak hilang: RMS output mirip input (0.5 → ~0.35 RMS).
        let rms = (produced.iter().map(|s| s * s).sum::<f32>() / produced.len() as f32).sqrt();
        assert!((0.2..0.6).contains(&rms), "RMS tidak wajar: {rms}");
    }

    #[test]
    fn output_comes_in_960_frame_blocks() {
        let mut r = StreamResampler::new(44_100, 2).unwrap();
        let input = sine(44_100, 200);
        let mut outs: Vec<usize> = Vec::new();
        for part in input.chunks(441 * 2) {
            let out = r.push(part).unwrap();
            if !out.is_empty() {
                outs.push(out.len() / 2);
            }
        }
        assert!(!outs.is_empty());
        assert!(
            outs.iter()
                .all(|f| *f == super::super::OUT_SAMPLES_PER_CHUNK),
            "semua blok harus 960 frame: {outs:?}"
        );
    }

    #[test]
    fn rejects_non_stereo_and_zero() {
        assert!(StreamResampler::new(0, 2).is_err());
        assert!(StreamResampler::new(48_000, 0).is_err());
        assert!(StreamResampler::new(44_100, 1).is_err());
    }
}
