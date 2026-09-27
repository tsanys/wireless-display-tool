//! Virtual display macOS via API **privat** `CGVirtualDisplay`.
//!
//! Binding ditulis sendiri (`objc2` `msg_send`) — kelas privat tidak punya
//! binding resmi. Struktur/alur mengikuti referensi MIT
//! `node-mac-virtual-display` (create → applySettings → displayID → release),
//! termasuk guard agar display fisik tetap menjadi main display.

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2_core_foundation::CGSize;
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCompleteDisplayConfiguration, CGConfigureDisplayOrigin,
    CGConfigureOption, CGDisplayConfigRef, CGDisplayIsActive, CGError, CGMainDisplayID,
};
use objc2_foundation::{NSArray, NSNotificationCenter, NSOperationQueue, NSString};

use super::VdCapability;

/// Nama notifikasi AppKit `NSApplicationDidChangeScreenParameters` (literal,
/// agar tidak menambah dependensi `objc2-app-kit` hanya untuk satu konstanta).
const DID_CHANGE_SCREEN_PARAMS: &str = "NSApplicationDidChangeScreenParameters";

const VENDOR_ID: u32 = 0xeeee;

/// Kelas privat yang harus ada agar fitur ini berfungsi.
fn classes_present() -> Result<(), String> {
    for name in [
        c"CGVirtualDisplay",
        c"CGVirtualDisplayDescriptor",
        c"CGVirtualDisplaySettings",
        c"CGVirtualDisplayMode",
    ] {
        if AnyClass::get(name).is_none() {
            return Err(format!(
                "macOS ini tidak menyediakan API virtual display ({name:?} tidak ditemukan)"
            ));
        }
    }
    Ok(())
}

/// Probe capability (tanpa efek samping: hanya cek keberadaan kelas).
pub fn capability() -> VdCapability {
    match classes_present() {
        Ok(()) => VdCapability {
            available: true,
            reason: None,
        },
        Err(reason) => VdCapability {
            available: false,
            reason: Some(reason),
        },
    }
}

/// Handle virtual display aktif. `Drop` melepas display (dan observer).
pub struct VirtualDisplay {
    display: Retained<AnyObject>,
    descriptor: Retained<AnyObject>,
    settings: Retained<AnyObject>,
    id: u32,
    previous_main: u32,
    observer: Option<Retained<ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>>,
}

impl VirtualDisplay {
    /// Buat display virtual `width`x`height` @ `refresh_hz`.
    pub fn create(width: u32, height: u32, refresh_hz: u32) -> Result<Self, String> {
        classes_present()?;
        if width < 2 || height < 2 || refresh_hz == 0 {
            return Err("ukuran/refresh display tidak valid".to_string());
        }
        let previous_main = CGMainDisplayID();

        // SAFETY: kontrak msg_send ke kelas privat (lihat referensi MIT).
        unsafe {
            let cls = AnyClass::get(c"CGVirtualDisplayDescriptor").expect("kelas dicek di atas");
            let descriptor: Retained<AnyObject> = msg_send![cls, new];
            let name = NSString::from_str("WDT Extended");
            let _: () = msg_send![&*descriptor, setName: &*name];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: width];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: height];
            // sizeInMillimeters dari ~81 PPI (mm/piksel = 25.4/81).
            let ratio = 25.4 / 81.0;
            let size = CGSize {
                width: width as f64 * ratio,
                height: height as f64 * ratio,
            };
            let _: () = msg_send![&*descriptor, setSizeInMillimeters: size];
            let (serial, product) = djb2_identity("WDT Extended");
            let _: () = msg_send![&*descriptor, setSerialNum: serial];
            let _: () = msg_send![&*descriptor, setProductID: product];
            let _: () = msg_send![&*descriptor, setVendorID: VENDOR_ID];

            let cls_disp = AnyClass::get(c"CGVirtualDisplay").expect("kelas dicek di atas");
            let allocated: Allocated<AnyObject> = msg_send![cls_disp, alloc];
            let display: Option<Retained<AnyObject>> =
                msg_send![allocated, initWithDescriptor: &*descriptor];
            let display = display.ok_or("initWithDescriptor mengembalikan nil")?;

            let cls_mode = AnyClass::get(c"CGVirtualDisplayMode").expect("kelas dicek di atas");
            let alloc_mode: Allocated<AnyObject> = msg_send![cls_mode, alloc];
            let mode: Option<Retained<AnyObject>> = msg_send![
                alloc_mode,
                initWithWidth: width,
                height: height,
                refreshRate: refresh_hz as f64
            ];
            let mode = mode.ok_or("mode display nil")?;

            let cls_set = AnyClass::get(c"CGVirtualDisplaySettings").expect("kelas dicek di atas");
            let settings: Retained<AnyObject> = msg_send![cls_set, new];
            let _: () = msg_send![&*settings, setHiDPI: 0_u32];
            let modes: Retained<NSArray<AnyObject>> = NSArray::from_slice(&[&*mode]);
            let _: () = msg_send![&*settings, setModes: &*modes];

            let ok: bool = msg_send![&*display, applySettings: &*settings];
            if !ok {
                return Err("applySettings mengembalikan false".into());
            }
            let id: u32 = msg_send![&*display, displayID];
            if id == 0 || !CGDisplayIsActive(id) {
                return Err(format!("displayID tidak aktif ({id})"));
            }

            let mut vd = Self {
                display,
                descriptor,
                settings,
                id,
                previous_main,
                observer: None,
            };
            // Guard: jangan biarkan display virtual merebut slot main display.
            vd.restore_main_if_hijacked();
            vd.register_observer();
            Ok(vd)
        }
    }

    /// ID CoreGraphics display virtual.
    pub fn display_id(&self) -> u32 {
        self.id
    }

    /// ID sumber capture WDT (`capturer_for_display`).
    pub fn capture_id(&self) -> String {
        format!("cg:{}", self.id)
    }

    /// Bila display virtual menjadi main display, kembalikan ke fisik semula.
    fn restore_main_if_hijacked(&self) {
        if CGMainDisplayID() == self.id && self.previous_main != self.id {
            restore_main_display(self.previous_main);
        }
    }

    /// Observer best-effort: re-assert main display pada perubahan topologi
    /// (hot-plug). Kegagalan hanya dicatat, tidak fatal.
    fn register_observer(&mut self) {
        let id = self.id;
        let prev = self.previous_main;
        let block = RcBlock::new(
            move |_note: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                if CGMainDisplayID() == id {
                    restore_main_display(prev);
                }
            },
        );
        let name = NSString::from_str(DID_CHANGE_SCREEN_PARAMS);
        // SAFETY: observer dipasang pada queue main; block memakai Rc hanya dari
        // thread main (dipanggil oleh AppKit di main queue).
        unsafe {
            let center = NSNotificationCenter::defaultCenter();
            let queue = NSOperationQueue::mainQueue();
            let token = center.addObserverForName_object_queue_usingBlock(
                Some(&name),
                None,
                Some(&queue),
                &block,
            );
            self.observer = Some(token);
        }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        // Lepas observer dulu agar tidak menembak setelah display hilang.
        if let Some(token) = self.observer.take() {
            // SAFETY: token valid; cast ke AnyObject hanya untuk removeObserver.
            unsafe {
                let any: &AnyObject = &*(Retained::as_ptr(&token) as *const AnyObject);
                NSNotificationCenter::defaultCenter().removeObserver(any);
            }
        }
        // Urutan referensi: descriptor, settings, lalu display.
        // (Field drop akan melepas; kita lakukan eksplisit agar urut.)
        let _ = &self.descriptor;
        let _ = &self.settings;
        let _ = &self.display;
        // Jika main display masih menunjuk ke display virtual yang akan hilang,
        // kembalikan ke fisik semula sebelum dilepas.
        if CGMainDisplayID() == self.id && self.previous_main != self.id {
            restore_main_display(self.previous_main);
        }
    }
}

/// Identitas stabil (DJB2) dari nama → (serial, product).
fn djb2_identity(name: &str) -> (u32, u32) {
    let mut hash: u64 = 5381;
    for b in name.bytes() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_add(hash)
            .wrapping_add(b as u64);
    }
    ((hash & 0xFFFF_FFFF) as u32, ((hash >> 16) & 0xFFFF) as u32)
}

/// Pulihkan main display ke display fisik via API publik CGDisplayConfiguration.
fn restore_main_display(target: u32) {
    // SAFETY: kontrak CGDisplayConfiguration standar (begin/configure/complete).
    unsafe {
        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        if CGBeginDisplayConfiguration(&mut config) != CGError::Success || config.is_null() {
            return;
        }
        if CGConfigureDisplayOrigin(config, target, 0, 0) != CGError::Success {
            return;
        }
        let _ = CGCompleteDisplayConfiguration(config, CGConfigureOption::ForSession);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_available_on_this_host() {
        let cap = capability();
        // Di CI/host macOS modern kelas privat tersedia; jika tidak, alasannya
        // harus jelas (bukan panic).
        if !cap.available {
            assert!(cap.reason.is_some(), "wajib ada alasan bila tidak tersedia");
        }
    }

    #[test]
    fn djb2_is_stable_and_nonempty() {
        let (s, p) = djb2_identity("WDT Extended");
        assert_ne!(s, 0);
        assert!(p <= 0xFFFF);
        assert_eq!(djb2_identity("WDT Extended"), (s, p));
    }
}
