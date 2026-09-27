//! Soak pipeline video (capture → letterbox → encode) **tanpa jaringan**.
//!
//! Mengisolasi karakteristik memori pipeline video (ScreenCaptureKit +
//! letterbox + VideoToolbox) dari jalur WebRTC/receiver — dipakai untuk
//! menyelidiki pertumbuhan RSS. Sampel RSS diambil dari luar (`ps`).
//!
//! ```sh
//! WDT_SOAK_SECS=120 cargo run --release -p wdt-core --example pipeline_soak
//! # di terminal lain: ps -o rss= -p <pid>  (berulang)
//! ```

use std::time::{Duration, Instant};

use wdt_core::capture::{ScreenCapturer, capturer_for_display};
use wdt_core::encode::{
    EncoderConfig, FrameEncoder, H264EntropyMode, H264Profile, default_encoder,
};
use wdt_core::stream::scale_letterbox_bgra;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .try_init()
        .ok();

    let secs: u64 = std::env::var("WDT_SOAK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let (w, h) = (1920u32, 1080u32);

    let mut capturer = match capturer_for_display("main") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("capturer gagal: {e}");
            std::process::exit(1);
        }
    };
    let mut encoder = match default_encoder(EncoderConfig {
        width: w,
        height: h,
        bitrate_bps: 10_000_000,
        fps: 30,
        keyframe_interval: 60,
        profile: H264Profile::Baseline,
        entropy_mode: Some(H264EntropyMode::Cavlc),
        quality: Some(0.9),
    }) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("encoder gagal: {e}");
            std::process::exit(1);
        }
    };

    let synth = std::env::var("WDT_SOAK_SYNTH").is_ok();
    let noenc = std::env::var("WDT_SOAK_NOENC").is_ok();
    // Buffer sintetis yang DIPAKAI ULANG (untuk mengisolasi alokasi capture).
    let synth_frame = wdt_core::capture::Frame {
        width: 1920,
        height: 1080,
        stride: 1920 * 4,
        format: wdt_core::capture::PixelFormat::Bgra8,
        data: vec![120u8; 1920 * 1080 * 4],
    };

    println!(
        "pid={} soak {secs}s @ {w}x{h} synth={synth}",
        std::process::id()
    );
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut frames = 0u64;
    let mut bytes = 0u64;
    let mut last_report = Instant::now();
    let frame_dur = Duration::from_nanos(1_000_000_000 / 30);
    let mut next = Instant::now();

    while Instant::now() < deadline {
        // Hipotesis: tanpa autorelease pool per-frame, ObjC temporaries dari
        // SCK/VideoToolbox menumpuk di thread ini.
        #[cfg(target_os = "macos")]
        let _pool = std::env::var("WDT_SOAK_POOL")
            .is_ok()
            .then(|| unsafe { objc2_foundation::NSAutoreleasePool::new() });
        next += frame_dur;
        let captured;
        let raw: &wdt_core::capture::Frame = if synth {
            &synth_frame
        } else {
            captured = match capturer.capture_frame() {
                Ok(f) => f,
                Err(_) => {
                    std::thread::sleep(frame_dur);
                    continue;
                }
            };
            &captured
        };
        if let Ok(scaled) = scale_letterbox_bgra(raw, w, h) {
            if !noenc {
                if let Ok(packets) = encoder.encode_frame(&scaled) {
                    for p in packets {
                        bytes += p.data.len() as u64;
                    }
                }
            }
            frames += 1;
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            println!(
                "frames={frames} fps={:.1} out_MB={:.1}",
                frames as f32 / (secs as f32 - (deadline - Instant::now()).as_secs_f32()).max(1.0),
                bytes as f32 / 1e6
            );
            last_report = Instant::now();
        }
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    println!("selesai: frames={frames} out_MB={:.1}", bytes as f32 / 1e6);
}
