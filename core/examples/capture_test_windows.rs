//! PoC capture 1 frame di Windows via Windows.Graphics.Capture.
//!
//! Run dari root repo DI MESIN WINDOWS:
//!     cargo run -p wdt-core --example capture_test_windows
//!
//! Output: core/examples/output/windows_capture_test.png — buka manual dan
//! pastikan isinya sesuai layar aktual saat capture dijalankan.
//!
//! Catatan: tidak ada prompt izin khusus; kalau berjalan di sesi RDP /
//! tanpa display aktif, example exit dengan pesan error yang jelas.

use wdt_core::capture::{Frame, ScreenCapturer, default_capturer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut capturer = default_capturer()?;
    let (w, h) = capturer.display_size();
    println!("display utama: {w}x{h}");

    let frame: Frame = capturer.capture_frame()?;
    println!(
        "frame: {}x{} stride={} format={:?} bytes={}",
        frame.width,
        frame.height,
        frame.stride,
        frame.format,
        frame.data.len()
    );

    // PNG butuh RGBA contiguous; Frame adalah BGRA dengan kemungkinan
    // stride > width*4, jadi salin per baris sambil tukar kanal R<->B.
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let row = &frame.data[y * frame.stride as usize..][..w as usize * 4];
        for px in row.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    let img: image::RgbaImage =
        image::RgbaImage::from_raw(w, h, rgba).expect("buffer RGBA tidak konsisten");

    let out = format!(
        "{}/examples/output/windows_capture_test.png",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::create_dir_all(format!("{}/examples/output", env!("CARGO_MANIFEST_DIR")))?;
    img.save(&out)?;
    println!("tersimpan: {out}");
    Ok(())
}
