//! Debug: cetak sdpMid/sdpMLineIndex kandidat ICE yang dihasilkan webrtc-rs.
//!   cargo run -p wdt-core --example print_candidate
//! Menentukan apakah kandidat yang di-relay ke libwebrtc punya mid yang benar
//! (libwebrtc menolak kandidat dengan mid tak dikenal: "JsepTransport doesn't exist").

use std::time::Duration;

use wdt_core::signaling::peer::{PeerEvent, SenderPeer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut peer = SenderPeer::video_only().await.map_err(|e| e.to_string())?;
    let _offer = peer.create_offer().await.map_err(|e| e.to_string())?;
    println!("menunggu kandidat ICE (5 dtk)...");
    for _ in 0..12 {
        match peer.next_event(Duration::from_millis(500)).await {
            Ok(PeerEvent::Ice(c)) => {
                println!(
                    "mid={:?} mline={} candidate={}",
                    c.sdp_mid, c.sdp_mline_index, c.candidate
                );
            }
            Ok(other) => println!("event lain: {other:?}"),
            Err(_) => {}
        }
    }
    Ok(())
}
