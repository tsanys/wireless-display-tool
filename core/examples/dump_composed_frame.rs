//! Dump 1 frame hasil compose (letterbox 1920x1080) ke PNG untuk inspeksi
//! ketajaman/letterbox tanpa perlu TV.
//!
//!   cargo run -p wdt-core --example dump_composed_frame
//! Output: core/examples/output/composed_frame.png

use wdt_core::capture::{ScreenCapturer, default_capturer};
use wdt_core::stream::scale_letterbox_bgra;

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut capturer = default_capturer()?;
    let (dw, dh) = capturer.display_size();
    let t0 = now_ms();
    let frame = capturer.capture_frame()?;
    let t1 = now_ms();
    println!("capture: {dw}x{dh} (data {} byte)", frame.data.len());
    println!("CAP{t0}_{t1}");

    // Compose ke ukuran panel TV.
    let composed = scale_letterbox_bgra(&frame, 1920, 1080)?;
    println!(
        "composed: {}x{} stride={}",
        composed.width, composed.height, composed.stride
    );

    // PNG butuh RGBA; frame BGRA -> tukar R<->B.
    let mut rgba = Vec::with_capacity(composed.data.len());
    for px in composed.data.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    let img = image::RgbaImage::from_raw(composed.width, composed.height, rgba)
        .expect("buffer RGBA tidak konsisten");
    let dir = format!("{}/examples/output", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir)?;
    let out = format!("{dir}/composed_frame.png");
    img.save(&out)?;
    println!("tersimpan: {out}");
    Ok(())
}
