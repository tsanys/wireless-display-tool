//! PoC integrasi T1+T2: capture 30 frame → encode H.264 → file .h264 mentah.
//!
//! Run dari root repo (di platform masing-masing):
//!     cargo run -p wdt-core --example capture_encode_test
//!
//! Output: core/examples/output/encode_test.h264 (Annex B, SPS/PPS tiap IDR).
//! Verifikasi dengan ffmpeg — lihat README section
//! "Development — Testing Encoding".

use std::io::Write;

use wdt_core::capture::{ScreenCapturer, default_capturer};
use wdt_core::encode::{EncoderConfig, FrameEncoder, default_encoder};

const N_FRAMES: u32 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut capturer = default_capturer()?;
    let (w, h) = capturer.display_size();
    println!("display: {w}x{h}");

    let mut encoder = default_encoder(EncoderConfig {
        width: w,
        height: h,
        ..Default::default()
    })?;
    println!("encoder siap (default: 5 Mbps, Main, IDR tiap 60 frame)");

    let out_path = format!(
        "{}/examples/output/encode_test.h264",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::create_dir_all(format!("{}/examples/output", env!("CARGO_MANIFEST_DIR")))?;
    let mut out = std::fs::File::create(&out_path)?;

    let mut total_bytes: usize = 0;
    let mut keyframes = 0u32;
    for i in 0..N_FRAMES {
        let frame = capturer.capture_frame()?;
        let packets = encoder.encode_frame(&frame)?;
        let mut frame_bytes = 0usize;
        for pkt in &packets {
            if pkt.is_keyframe {
                keyframes += 1;
            }
            frame_bytes += pkt.data.len();
            out.write_all(&pkt.data)?;
        }
        total_bytes += frame_bytes;
        println!("frame {i}: {} paket, {frame_bytes} byte", packets.len());
    }
    for pkt in encoder.flush()? {
        if pkt.is_keyframe {
            keyframes += 1;
        }
        total_bytes += pkt.data.len();
        out.write_all(&pkt.data)?;
    }

    println!("selesai: {N_FRAMES} frame, {total_bytes} byte, {keyframes} keyframe");
    println!("tersimpan: {out_path}");
    Ok(())
}
