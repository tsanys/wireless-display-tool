//! Debug: cetak SDP offer yang dihasilkan webrtc-rs (SenderPeer).
//!   cargo run -p wdt-core --example print_offer
//! Dipakai untuk membandingkan atribut transport (a=rtcp-mux, a=group:BUNDLE)
//! terhadap yang diharapkan libwebrtc Android.

use wdt_core::signaling::peer::SenderPeer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let peer = SenderPeer::new().await.map_err(|e| e.to_string())?;
    let offer = peer.create_offer().await.map_err(|e| e.to_string())?;
    println!("{offer}");
    Ok(())
}
