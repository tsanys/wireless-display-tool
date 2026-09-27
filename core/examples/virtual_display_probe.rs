//! PoC R5a/R5b: virtual display macOS via modul `wdt_core::vdisplay`.
//!
//! Assertion berurutan (lihat docs/R5_DECISIONS.md):
//! 1. Display virtual 1920×1080@60 dibuat (API privat CoreGraphics).
//! 2. Muncul di daftar display aktif CG + `available_displays()` WDT.
//! 3. Muncul di `SCShareableContent` ScreenCaptureKit (interop kritis).
//! 4. Tertangkap pipeline capture WDT (`capturer_for_display`) → frame nyata.
//! 5. Dilepas bersih (tidak ada yatim; main display tidak berubah).
//!
//! ```sh
//! cargo run -p wdt-core --example virtual_display_probe
//! ```

#[cfg(target_os = "macos")]
mod imp {
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2_foundation::NSError;
    use objc2_screen_capture_kit::SCShareableContent;
    use wdt_core::capture::{ScreenCapturer, available_displays, capturer_for_display};

    pub fn run() {
        println!("== PoC virtual display macOS (modul wdt_core::vdisplay) ==");
        let cap = wdt_core::vdisplay::capability();
        println!(
            "capability: available={} reason={:?}",
            cap.available, cap.reason
        );
        if !cap.available {
            eprintln!("GAGAL: fitur tidak tersedia di host ini");
            std::process::exit(2);
        }
        let before = core_graphics::display::CGDisplay::active_displays().unwrap_or_default();
        println!("display aktif sebelum: {before:?}");

        let mut fail = 0usize;

        // 1. Buat.
        let vd = match wdt_core::vdisplay::VirtualDisplay::create(1920, 1080, 60) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("GAGAL langkah 1: {e}");
                std::process::exit(1);
            }
        };
        let id = vd.display_id();
        println!(
            "LANGKAH 1 PASS: displayID={id} capture_id={}",
            vd.capture_id()
        );

        // 2. Terlihat CG + WDT.
        let mut in_active = false;
        let mut in_wdt = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let act = core_graphics::display::CGDisplay::active_displays().unwrap_or_default();
            in_active = act.contains(&id);
            in_wdt = available_displays()
                .map(|ds| ds.iter().any(|d| d.id == vd.capture_id()))
                .unwrap_or(false);
            if in_active && in_wdt {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        println!("  active={in_active} wdt={in_wdt}");
        if !in_active || !in_wdt {
            eprintln!("GAGAL langkah 2");
            fail += 1;
        } else {
            println!("LANGKAH 2 PASS");
        }

        // 3. Terlihat SCK.
        let mut in_sck = false;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if let Some(content) = get_shareable_content() {
                // SAFETY: accessor standar pada objek SCK.
                let displays = unsafe { content.displays() };
                for i in 0..displays.count() {
                    // SAFETY: index < count.
                    if unsafe { displays.objectAtIndex(i).displayID() } == id {
                        in_sck = true;
                        break;
                    }
                }
                if in_sck {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        if !in_sck {
            eprintln!("GAGAL langkah 3: SCK tidak melihatnya");
            fail += 1;
        } else {
            println!("LANGKAH 3 PASS: terlihat ScreenCaptureKit");
        }

        // 4. Capture.
        let mut captured = 0usize;
        if in_sck {
            match capturer_for_display(&vd.capture_id()) {
                Ok(mut c) => {
                    let d = Instant::now() + Duration::from_secs(6);
                    while captured < 5 && Instant::now() < d {
                        if let Ok(f) = c.capture_frame() {
                            println!("  frame {}: {}x{}", captured + 1, f.width, f.height);
                            captured += 1;
                        }
                    }
                }
                Err(e) => eprintln!("  capturer error: {e}"),
            }
        }
        if captured == 0 {
            eprintln!("GAGAL langkah 4");
            fail += 1;
        } else {
            println!("LANGKAH 4 PASS: {captured} frame");
        }

        // 5. Lepas bersih.
        drop(vd);
        std::thread::sleep(Duration::from_millis(800));
        let act = core_graphics::display::CGDisplay::active_displays().unwrap_or_default();
        let gone = !act.contains(&id);
        let main_ok = act.len() == before.len();
        println!("  hilang={gone} jumlah_aktif_kembali_normal={main_ok}");
        if !gone || !main_ok {
            eprintln!("GAGAL langkah 5");
            fail += 1;
        } else {
            println!("LANGKAH 5 PASS: dilepas bersih");
        }

        println!(
            "== HASIL: {} ==",
            if fail == 0 { "SEMUA PASS" } else { "ADA GAGAL" }
        );
        std::process::exit(if fail == 0 { 0 } else { 1 });
    }

    fn get_shareable_content() -> Option<Retained<SCShareableContent>> {
        let (tx, rx) = std::sync::mpsc::channel::<Result<Retained<SCShareableContent>, String>>();
        let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
        let block = block2::StackBlock::new({
            let tx = std::rc::Rc::clone(&tx);
            move |content: *mut SCShareableContent, err: *mut NSError| {
                let result = if content.is_null() {
                    Err(unsafe { err.as_ref() }
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "unknown".to_string()))
                } else {
                    // SAFETY: pointer non-null valid selama callback.
                    Ok(unsafe { Retained::retain(content).unwrap() })
                };
                if let Some(tx) = tx.borrow_mut().take() {
                    let _ = tx.send(result);
                }
            }
        });
        let block_ref: &block2::Block<dyn Fn(*mut SCShareableContent, *mut NSError)> = &block;
        // SAFETY: block hidup sampai pump selesai.
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                false, true, block_ref,
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(v) = rx.try_recv() {
                return v.ok();
            }
            if Instant::now() >= deadline {
                return None;
            }
            // SAFETY: mode default runloop valid.
            let mode = unsafe { objc2_core_foundation::kCFRunLoopDefaultMode };
            objc2_core_foundation::CFRunLoop::run_in_mode(mode, 0.01, false);
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    imp::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("virtual_display_probe hanya untuk macOS");
}
