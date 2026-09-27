//! Virtual display (Extended) — **eksperimental**.
//!
//! Implementasi per-platform hidup di submodul. Saat ini hanya macOS (via API
//! **privat** `CGVirtualDisplay`). Lihat `docs/R5_DECISIONS.md`: PoC membuktikan
//! display virtual terlihat oleh ScreenCaptureKit dan dapat ditangkap pipeline
//! WDT, lalu dilepas bersih.
//!
//! PERINGATAN: API privat dapat berubah pada major release macOS. Modul ini
//! menjaga blast radius tetap kecil dan selalu gagal dengan pesan jelas
//! (bukan crash). Fitur ini default **off** di UI (capability-gated).

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{VirtualDisplay, capability};

/// Kemampuan membuat virtual display di platform berjalan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VdCapability {
    pub available: bool,
    /// Alasan user-facing bila tidak tersedia.
    pub reason: Option<String>,
}

#[cfg(not(target_os = "macos"))]
pub fn capability() -> VdCapability {
    VdCapability {
        available: false,
        reason: Some(
            "Layar tambahan saat ini hanya tersedia di macOS (eksperimental). Windows \
             memerlukan driver IddCx yang belum dirilis."
                .to_string(),
        ),
    }
}
