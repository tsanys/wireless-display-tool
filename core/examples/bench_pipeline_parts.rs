//! Benchmark bagian pipeline: capture vs downscale vs encode (host).
//!   cargo run --release -p wdt-core --example bench_pipeline_parts
//!
//! Dipakai untuk memutuskan optimasi (mis. perlu SCStream persisten atau tidak).

use std::time::Instant;

use wdt_core::capture::{ScreenCapturer, default_capturer};
use wdt_core::encode::{EncoderConfig, FrameEncoder, H264Profile, default_encoder};
use wdt_core::stream::downscale_bgra;

const N: usize = 60;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut capturer = default_capturer()?;
    let (dw, dh) = capturer.display_size();
    println!("display: {dw}x{dh}");

    // --- capture saja ---
    let mut frames = Vec::with_capacity(N);
    let mut capture_total = std::time::Duration::ZERO;
    for _ in 0..N {
        let t = Instant::now();
        let f = capturer.capture_frame()?;
        capture_total += t.elapsed();
        frames.push(f);
    }
    let capture_ms = capture_total.as_secs_f64() * 1000.0 / N as f64;

    // --- downscale saja (ke 720p) ---
    let target_w = dw.min(1280) & !1;
    let target_h = dh.min(720) & !1;
    let mut scaled = Vec::with_capacity(N);
    let mut scale_total = std::time::Duration::ZERO;
    for f in &frames {
        let t = Instant::now();
        let s = downscale_bgra(f, target_w, target_h)?;
        scale_total += t.elapsed();
        scaled.push(s);
    }
    let scale_ms = scale_total.as_secs_f64() * 1000.0 / N as f64;

    // --- encode saja ---
    let mut encoder = default_encoder(EncoderConfig {
        width: target_w,
        height: target_h,
        bitrate_bps: 10_000_000,
        fps: 30,
        keyframe_interval: 60,
        profile: H264Profile::Baseline,
        entropy_mode: None,
        quality: None,
    })?;
    let mut encode_total = std::time::Duration::ZERO;
    let mut bytes = 0usize;
    for s in &scaled {
        let t = Instant::now();
        let pkts = encoder.encode_frame(s)?;
        encode_total += t.elapsed();
        bytes += pkts.iter().map(|p| p.data.len()).sum::<usize>();
    }
    let encode_ms = encode_total.as_secs_f64() * 1000.0 / N as f64;

    println!("target: {target_w}x{target_h}, N={N}");
    println!(
        "capture : {capture_ms:6.1} ms/frame  ({:.1} fps)",
        1000.0 / capture_ms
    );
    println!("downscale:{scale_ms:6.1} ms/frame");
    println!("encode  : {encode_ms:6.1} ms/frame");
    println!(
        "total   : {:6.1} ms/frame -> max {:.1} fps (tanpa sink)",
        capture_ms + scale_ms + encode_ms,
        1000.0 / (capture_ms + scale_ms + encode_ms)
    );
    println!("bytes total encode: {bytes}");
    Ok(())
}
