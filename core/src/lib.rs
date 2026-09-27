//! wdt-core — Rust core untuk wireless-display-tool.
//!
//! Berisi capture layar, hardware encoding H.264, signaling WebRTC, dan
//! pipeline streaming (capture → encode → track).

pub mod audio;
pub mod capture;
pub mod encode;
pub mod signaling;
pub mod stream;
pub mod vdisplay;
