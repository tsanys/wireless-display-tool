//! Embedded signaling server + WebRTC peer + mDNS discovery (T3).
//!
//! Kontrak pesan: [`protocol`] (single source of truth, didokumentasikan
//! di `docs/SIGNALING_PROTOCOL.md`).

pub mod discovery;
pub mod peer;
pub mod protocol;
pub mod sdp_util;
pub mod sender_session;
pub mod server;
