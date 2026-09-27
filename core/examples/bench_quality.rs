//! Ukur encode 1920x1080 constant-quality (T6 Q2/Q3).
//!
//!   cargo run -p wdt-core --example bench_quality
//!   WDT_BENCH_SYNTHETIC=1 cargo run -p wdt-core --example bench_quality
//!   WDT_H264_PROFILE=high WDT_H264_CABAC=1 cargo run -p wdt-core --example bench_quality
//!
//! Membandingkan bitrate nyata vs mode bitrate lama, dan memastikan
//! property Quality + cap diterima VT (lihat warning bila tidak). Benchmark
//! memakai frame layar asli bila izin tersedia, atau pola sintetis
//! deterministik bila proses tidak punya izin Screen Recording.

use std::time::Instant;

use wdt_core::capture::{Frame, PixelFormat, ScreenCapturer, default_capturer};
use wdt_core::encode::{EncoderConfig, FrameEncoder, H264Profile, H264Tuning, default_encoder};
use wdt_core::stream::scale_letterbox_bgra;

const FRAMES: u32 = 120;
const FPS: u32 = 30;

/// Pola high-detail deterministik untuk benchmark tanpa izin screen capture.
fn synthetic_frame(width: u32, height: u32) -> Frame {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    let mut state = 0x9e37_79b9u32;
    for y in 0..height {
        for x in 0..width {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let grid = if (x / 8 + y / 8) % 2 == 0 { 0x30 } else { 0xd0 };
            data.extend_from_slice(&[
                (state as u8) ^ grid,
                ((state >> 8) as u8) ^ grid,
                ((state >> 16) as u8) ^ grid,
                0xff,
            ]);
        }
    }
    Frame {
        width,
        height,
        stride: width * 4,
        format: PixelFormat::Bgra8,
        data,
    }
}

fn benchmark_input() -> Frame {
    if std::env::var_os("WDT_BENCH_SYNTHETIC").is_some() {
        println!("input: synthetic high-detail 1920x1080");
        return synthetic_frame(1920, 1080);
    }
    let captured = default_capturer().and_then(|mut capturer| capturer.capture_frame());
    match captured {
        Ok(frame) => match scale_letterbox_bgra(&frame, 1920, 1080) {
            Ok(composed) => {
                println!("input: screen capture + letterbox 1920x1080");
                composed
            }
            Err(e) => {
                eprintln!("compose gagal ({e}); memakai frame sintetis");
                synthetic_frame(1920, 1080)
            }
        },
        Err(e) => {
            eprintln!("screen capture tidak tersedia ({e}); memakai frame sintetis");
            synthetic_frame(1920, 1080)
        }
    }
}

fn tuning_override() -> Result<Option<H264Tuning>, String> {
    let profile = std::env::var("WDT_H264_PROFILE").ok();
    let cabac = std::env::var("WDT_H264_CABAC").ok();
    if profile.is_none() && cabac.is_none() {
        return Ok(None);
    }
    H264Tuning::parse(profile.as_deref(), cabac.as_deref()).map(Some)
}

/// Geser isi frame horizontal `dx` px (wrap), untuk mensimulasi gerakan.
fn shift_x(frame: &Frame, dx: i32) -> Frame {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut out = vec![0u8; frame.data.len()];
    for y in 0..h {
        for x in 0..w {
            let sx = ((x as i32 + dx) % w as i32) as usize;
            let src = (y * w + sx) * 4;
            let dst = (y * w + x) * 4;
            out[dst..dst + 4].copy_from_slice(&frame.data[src..src + 4]);
        }
    }
    Frame {
        width: frame.width,
        height: frame.height,
        stride: frame.stride,
        format: frame.format,
        data: out,
    }
}

fn run(label: &str, quality: Option<f32>) -> Result<(), Box<dyn std::error::Error>> {
    let composed = benchmark_input();
    let tuning = tuning_override()?;
    if let Some(tuning) = tuning {
        println!("tuning: {:?} + {:?}", tuning.profile, tuning.entropy_mode);
    }

    let mut encoder = default_encoder(EncoderConfig {
        width: 1920,
        height: 1080,
        // Dalam quality mode nilai ini menjadi cap bitrate.
        bitrate_bps: 10_000_000,
        fps: FPS,
        profile: tuning.map_or(H264Profile::Main, |t| t.profile),
        entropy_mode: tuning.map(|t| t.entropy_mode),
        quality,
        ..Default::default()
    })?;

    let mut total = 0usize;
    let mut packets = 0u32;
    let mut first_idr = 0usize;
    let start = Instant::now();
    for i in 0..FRAMES {
        // Simulasi gerakan: geser konten 4 px per frame (fotbal/video).
        let shifted = shift_x(&composed, (i as i32 * 4) % composed.width as i32);
        for pkt in encoder.encode_frame(&shifted)? {
            if first_idr == 0 && pkt.is_keyframe {
                first_idr = pkt.data.len();
            }
            total += pkt.data.len();
            packets += 1;
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let ms = start.elapsed().as_millis();
    let media_secs = FRAMES as f64 / FPS as f64;
    let kbps = total as f64 * 8.0 / media_secs / 1000.0;
    println!(
        "{label}: {FRAMES} frame dalam {ms} ms ({:.1} fps), {packets} paket, \
         rata-rata {:.0} kB, ~{kbps:.0} kbps",
        FRAMES as f64 / secs,
        total as f64 / FRAMES as f64 / 1000.0,
    );
    println!("  IDR pertama: {first_idr} byte");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run("bitrate 10 Mbps", None)?;
    run("quality 0.80", Some(0.8))?;
    run("quality 0.90", Some(0.9))?;
    Ok(())
}
