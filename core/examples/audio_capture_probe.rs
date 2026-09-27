//! Probe capture audio sistem: merekam ±3 detik lalu melaporkan statistik.
//!
//! Dijalankan di host untuk memverifikasi backend capture (macOS
//! ScreenCaptureKit / Windows WASAPI) benar-benar menghasilkan PCM —
//! bukan bagian dari aplikasi.
//!
//! ```sh
//! cargo run -p wdt-core --example audio_capture_probe
//! ```
//!
//! macOS: butuh izin Screen Recording (sama seperti capture layar).

use std::time::{Duration, Instant};

use wdt_core::audio::{AudioCaptureError, SystemAudioCapturer};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cap = wdt_core::audio::capability();
    println!(
        "capability: available={} reason={:?}",
        cap.available, cap.reason
    );
    if !cap.available {
        eprintln!("capture audio tidak tersedia; keluar");
        std::process::exit(2);
    }

    let mut capturer = match wdt_core::audio::default_capturer() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gagal membuat capturer: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "capturer: {} Hz, {} kanal",
        capturer.sample_rate(),
        capturer.channels()
    );

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut frames = 0u64;
    let mut samples = 0u64;
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    let mut missing = 0u64;
    let mut last_ts = Duration::ZERO;

    while Instant::now() < deadline {
        match capturer.read_frame() {
            Ok(frame) => {
                frames += 1;
                samples += frame.samples.len() as u64;
                last_ts = frame.timestamp;
                for s in &frame.samples {
                    peak = peak.max(s.abs());
                    sum_sq += (*s as f64) * (*s as f64);
                }
            }
            Err(AudioCaptureError::Timeout(_)) => missing += 1,
            Err(e) => {
                eprintln!("error baca: {e}");
                std::process::exit(1);
            }
        }
    }

    let rms = if samples > 0 {
        (sum_sq / samples as f64).sqrt() as f32
    } else {
        0.0
    };
    println!(
        "hasil: {frames} frame, {samples} sampel, {missing} timeout, \
         peak={peak:.3} rms={rms:.4}, ts_akhir={last_ts:?}"
    );
    #[cfg(target_os = "macos")]
    {
        let (cb, built, failed) = wdt_core::audio::macos::debug_stats();
        println!("scK: callback={cb} built={built} build_gagal={failed}");
        println!("scK stages: {:?}", wdt_core::audio::macos::debug_stages());
    }
    // Bukti capture nyata: ada sampel dari device (boleh senyap → peak kecil,
    // tetapi jumlah sampel > 0 membuktikan pipeline SCK/WASAPI hidup).
    assert!(samples > 0, "tidak ada sampel: capture tidak bekerja");
    println!("OK: capture audio sistem menghasilkan data");
}
