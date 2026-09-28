//! Test client signaling T3 (dua proses: sender & receiver).
//!
//! Terminal 1 (server demo, catat token):
//!     cargo run -p wdt-core --example signaling_server_demo 8431
//! Terminal 2:
//!     cargo run -p wdt-core --example signaling_test_client -- --role sender --url ws://127.0.0.1:8431/ws
//! Terminal 3:
//!     cargo run -p wdt-core --example signaling_test_client -- --role receiver --url ws://127.0.0.1:8431/ws --token <TOKEN>
//!
//! Sukses = kedua proses exit 0 setelah tukar "ping"→"pong" via DataChannel.
//!
//! Sisi sender memakai modul reusable `sender_session` (sama yang dipakai
//! src-tauri T4); sisi receiver memakai `ReceiverPeer` langsung.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use wdt_core::signaling::peer::{PeerEvent, ReceiverPeer};
use wdt_core::signaling::protocol::{ClientMsg, PROTO_VERSION, Role, ServerMsg};
use wdt_core::signaling::sender_session::{self, SenderMode, SessionCmd, SessionEvent};

fn usage() -> ! {
    eprintln!(
        "usage: signaling_test_client --role sender|receiver --url ws://host:port/ws [--token 123456] [--device-id NAME]"
    );
    std::process::exit(2);
}

fn arg(name: &str) -> Option<String> {
    let mut it = std::env::args().skip(1).peekable();
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
    }
    None
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let role = arg("--role").unwrap_or_else(|| usage());

    if role == "sender" {
        let url = arg("--url").unwrap_or_else(|| usage());
        // Default WithCtrl (Test A T3: ping/pong); --video-only untuk uji
        // interop TV (offer hanya m=video).
        let mode = if std::env::args().any(|a| a == "--video-only") {
            SenderMode::VideoOnly
        } else {
            SenderMode::WithCtrl
        };
        return run_sender_session(&url, mode).await;
    }

    // --- receiver (langsung pakai ReceiverPeer + WS manual) ---
    let url = arg("--url").unwrap_or_else(|| usage());
    let token = arg("--token");
    let device_id = arg("--device-id");
    let device_name = arg("--device-name");

    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    println!("[receiver] WS terhubung ke {url}");
    let (mut sink, mut stream) = ws.split();

    send_msg(
        &mut sink,
        &ClientMsg::Hello {
            role: Role::Receiver,
            proto: PROTO_VERSION,
            token,
            device_id,
            device_name,
            caps: None,
        },
    )
    .await?;
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<ServerMsg>(t.as_str())? {
                ServerMsg::HelloOk { server, .. } => {
                    println!("[receiver] helloOk dari {server}");
                    break;
                }
                ServerMsg::Error { code, message } => {
                    eprintln!("[receiver] hello ditolak: {code:?} {message}");
                    std::process::exit(1);
                }
                other => {
                    eprintln!("[receiver] respons tak terduga: {other:?}");
                    std::process::exit(1);
                }
            },
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => {
                eprintln!("[receiver] WS tertutup sebelum helloOk");
                std::process::exit(1);
            }
        }
    }

    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<ServerMsg>();
    tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if let Ok(parsed) = serde_json::from_str::<ServerMsg>(t.as_str())
                        && sig_tx.send(parsed).is_err()
                    {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    if !run_receiver(&mut sink, &mut sig_rx).await {
        eprintln!("[receiver] TEST GAGAL");
        std::process::exit(1);
    }
    println!("[receiver] TEST LULUS");
    let _ = send_msg(
        &mut sink,
        &ClientMsg::Bye {
            reason: Some("test-done".to_string()),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

/// Sumber gambar dari env: WDT_EXTENDED="1920x1080[@60]" → display virtual
/// (Extended, eksperimental); default display utama.
fn capture_target() -> wdt_core::signaling::sender_session::CaptureTarget {
    use wdt_core::signaling::sender_session::CaptureTarget;
    if let Ok(spec) = std::env::var("WDT_EXTENDED") {
        let (wh, hz) = match spec.split_once('@') {
            Some((wh, hz)) => (wh.to_string(), hz.trim().parse::<u32>().unwrap_or(60)),
            None => (spec.clone(), 60),
        };
        if let Some((w, h)) = wh.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                return CaptureTarget::Virtual {
                    width: w,
                    height: h,
                    refresh_hz: hz.max(1),
                };
            }
        }
        eprintln!("[sender] WDT_EXTENDED tidak valid: {spec:?} (pakai WxH[@Hz])");
    }
    CaptureTarget::Display("main".to_string())
}

/// Pengaturan video dari env: WDT_VIDEO=1080p|720p, WDT_FPS_UI=30|60,
/// WDT_QUALITY=balanced|sharp (default 1080p30 balanced).
fn sender_video_settings() -> wdt_core::signaling::sender_session::VideoSettings {
    use wdt_core::signaling::sender_session::{QualityPreset, ResolutionPreset, VideoSettings};
    let resolution = match std::env::var("WDT_VIDEO").ok().as_deref() {
        Some("720p") => ResolutionPreset::P720,
        _ => ResolutionPreset::P1080,
    };
    let fps = std::env::var("WDT_FPS_UI")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|f| *f == 30 || *f == 60)
        .unwrap_or(30);
    let quality = match std::env::var("WDT_QUALITY").ok().as_deref() {
        Some("sharp") => QualityPreset::Sharp,
        _ => QualityPreset::Balanced,
    };
    VideoSettings {
        resolution,
        fps,
        quality,
    }
}

/// Route audio dari env: WDT_AUDIO=tv|both|muted (default laptop).
fn sender_audio_route() -> wdt_core::signaling::protocol::AudioRoute {
    use wdt_core::signaling::protocol::AudioRoute;
    match std::env::var("WDT_AUDIO").ok().as_deref().map(str::trim) {
        Some("tv") => AudioRoute::Tv,
        Some("both") => AudioRoute::Both,
        Some("muted") => AudioRoute::Muted,
        _ => AudioRoute::Laptop,
    }
}

/// Sender via modul reusable sender_session (jalur sama dgn src-tauri).
async fn run_sender_session(url: &str, mode: SenderMode) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = sender_session::spawn_sender_session(url, mode)
        .await
        .map_err(|e| e.to_string())?;
    println!("[sender] helloOk, menunggu receiver...");

    // Tunggu receiver bergabung (30 dtk; 300 dtk saat WDT_NO_C1 agar
    // receiver bisa connect tanpa sender keburu one-shot offer).
    let wait_secs = if std::env::var("WDT_NO_C1").is_ok() {
        300
    } else {
        30
    };
    let joined = tokio::time::timeout(Duration::from_secs(wait_secs), async {
        loop {
            match session.events.recv().await {
                Some(SessionEvent::ReceiverJoined { device_id, .. }) => {
                    println!("[sender] receiver masuk: {device_id:?}");
                    break;
                }
                Some(SessionEvent::Error(e)) => {
                    eprintln!("[sender] error sesi: {e}");
                    std::process::exit(1);
                }
                Some(_) => continue,
                None => {
                    eprintln!("[sender] event channel tertutup");
                    std::process::exit(1);
                }
            }
        }
    })
    .await;
    if joined.is_err() {
        println!("[sender] timeout tunggu receiver — kirim offer langsung");
    }

    session
        .cmds
        .send(SessionCmd::StartOffer {
            target: capture_target(),
            audio_route: sender_audio_route(),
            video: sender_video_settings(),
        })
        .map_err(|_| "sesi mati".to_string())?;

    // WDT_NO_C1 → sesi panjang untuk pengukuran (bukan uji otomatis).
    let deadline_secs = if std::env::var("WDT_NO_C1").is_ok() {
        3600
    } else {
        60
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(deadline_secs);
    let mut ping_sent = false;
    // Mode video-only: anggap sukses setelah pipeline benar-benar mengirim
    // sample (stats frames > 0), bukan sekadar "connected".
    let mut frames_seen: u64 = 0;
    let mut hold_until: Option<tokio::time::Instant> = None;
    // Verifikasi C1: setelah streaming sukses, Stop lalu Start lagi — sesi
    // HARUS tetap hidup (dulu: "sesi sender mati" permanen).
    let mut verify_restart = false;
    let mut restart_phase = 0u8; // 0=streaming 1=stopping 2=restart-offered
    let second_stream_ok = false;
    // Uji live switching: WDT_AUDIO_CYCLE=1 → ubah route tiap 15 dtk
    // (Tv → Laptop → Both → Tv) tanpa memutus video.
    let cycle = std::env::var("WDT_AUDIO_CYCLE").is_ok();
    let mut next_switch = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut cycle_idx = 0usize;

    loop {
        if tokio::time::Instant::now() > deadline {
            eprintln!("[sender] timeout handshake/streaming");
            std::process::exit(1);
        }
        if cycle && frames_seen > 0 && tokio::time::Instant::now() >= next_switch {
            use wdt_core::signaling::protocol::AudioRoute;
            let routes = [
                AudioRoute::Laptop,
                AudioRoute::Tv,
                AudioRoute::Both,
                AudioRoute::Tv,
            ];
            let r = routes[cycle_idx % routes.len()];
            println!("[sender] live-switch → {r:?} (video harus tetap)");
            session.cmds.send(SessionCmd::SetAudioRoute(r)).ok();
            cycle_idx += 1;
            next_switch = tokio::time::Instant::now() + Duration::from_secs(15);
        }
        if let Some(until) = hold_until
            && tokio::time::Instant::now() >= until
            && restart_phase == 0
        {
            if frames_seen == 0 {
                eprintln!("[sender] TEST GAGAL: pipeline tidak mengirim frame");
                std::process::exit(1);
            }
            if verify_restart {
                // Fase 1: Stop mirroring (sesi WS harus tetap hidup).
                println!("[sender] fase-C1: StopMirroring…");
                session.cmds.send(SessionCmd::StopMirroring).ok();
                restart_phase = 1;
                hold_until = Some(tokio::time::Instant::now() + Duration::from_secs(2));
            } else {
                session.cmds.send(SessionCmd::Shutdown).ok();
                tokio::time::sleep(Duration::from_millis(300)).await;
                println!("[sender] total {frames_seen} frame terkirim");
                println!("[sender] TEST LULUS");
                return Ok(());
            }
        } else if let Some(until) = hold_until
            && tokio::time::Instant::now() >= until
            && restart_phase == 1
        {
            // Fase 2: Start lagi — receiver (test client) sudah keluar, jadi
            // server akan balas error recoverable. Sesi HARUS tetap hidup
            // (dulu: mati permanen -> "sesi sender mati").
            println!("[sender] fase-C1: StartOffer ulang (tanpa receiver)…");
            session
                .cmds
                .send(SessionCmd::StartOffer {
                    target: capture_target(),
                    audio_route: sender_audio_route(),
                    video: sender_video_settings(),
                })
                .ok();
            restart_phase = 2;
            hold_until = Some(tokio::time::Instant::now() + Duration::from_secs(3));
        } else if let Some(until) = hold_until
            && tokio::time::Instant::now() >= until
            && restart_phase == 2
        {
            session.cmds.send(SessionCmd::Shutdown).ok();
            tokio::time::sleep(Duration::from_millis(300)).await;
            if second_stream_ok {
                println!("[sender] total {frames_seen} frame terkirim");
                println!("[sender] TEST LULUS (termasuk C1: sesi selamat Stop→Start)");
                return Ok(());
            }
            println!(
                "[sender] total {frames_seen} frame; sesi selamat Stop→Start (C1 OK,                  stream kedua tidak diuji di host — butuh receiver re-arm di device)"
            );
            println!("[sender] TEST LULUS");
            return Ok(());
        }
        match tokio::time::timeout(Duration::from_millis(500), session.events.recv()).await {
            Ok(Some(SessionEvent::OfferSent)) => println!("[sender] offer terkirim"),
            Ok(Some(SessionEvent::AnswerReceived)) => println!("[sender] answer diterima"),
            Ok(Some(SessionEvent::PipelineStarted {
                width,
                height,
                fps,
                ssrc,
                payload_type,
            })) => {
                println!(
                    "[sender] pipeline mulai: {width}x{height}@{fps} ssrc={ssrc} pt={payload_type}"
                );
            }
            Ok(Some(SessionEvent::PipelineStats(st))) => {
                frames_seen = frames_seen.max(st.frames);
                println!(
                    "[sender] stats: {:.1} fps, {} frame, {} skipped, {} error",
                    st.fps, st.frames, st.skipped, st.errors
                );
            }
            Ok(Some(SessionEvent::MirroringStopped)) => {
                println!("[sender] mirroring berhenti");
            }
            Ok(Some(SessionEvent::AudioStats(st))) => {
                println!(
                    "[sender] audio: {} paket, {} ms, level {:.4}, {} error, {} reconnect",
                    st.packets_sent, st.sent_ms, st.level, st.errors, st.reconnects
                );
            }
            Ok(Some(SessionEvent::AudioRouteChanged { route })) => {
                println!("[sender] route audio aktif: {route:?}");
            }
            Ok(Some(SessionEvent::AudioDegraded { message })) => {
                println!("[sender] audio degraded: {message}");
            }
            Ok(Some(SessionEvent::ReceiverLeft { reason })) => {
                println!("[sender] receiver pergi: {reason} (sesi tetap hidup)");
            }
            Ok(Some(SessionEvent::PeerConnected)) => {
                println!("[sender] peer connected");
                if mode == SenderMode::VideoOnly {
                    // Observasi stats streaming ~8 dtk (loop tetap mengonsumsi
                    // event supaya stats tidak tertahan di antrean).
                    // WDT_NO_C1=1 → stream terus-menerus (untuk pengukuran
                    // latency A/B), tanpa uji Stop→Start otomatis.
                    if std::env::var("WDT_NO_C1").is_ok() {
                        println!("[sender] streaming terus (WDT_NO_C1)");
                    } else {
                        println!("[sender] streaming… (observasi stats ~8 dtk)");
                        hold_until = Some(tokio::time::Instant::now() + Duration::from_secs(8));
                        verify_restart = true;
                    }
                }
            }
            Ok(Some(SessionEvent::CtrlOpen)) => {
                if !ping_sent {
                    ping_sent = true;
                    println!("[sender] ctrl open → kirim ping");
                    session.cmds.send(SessionCmd::SendCtrl("ping".into())).ok();
                }
            }
            Ok(Some(SessionEvent::CtrlMessage(m))) => {
                if m == "pong" {
                    println!("[sender] pong diterima");
                    println!("[sender] TEST LULUS");
                    session.cmds.send(SessionCmd::Shutdown).ok();
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    return Ok(());
                }
            }
            Ok(Some(SessionEvent::Error(e))) => {
                eprintln!("[sender] error: {e}");
                std::process::exit(1);
            }
            Ok(Some(SessionEvent::Ended { reason })) => {
                if restart_phase > 0 {
                    eprintln!("[sender] TEST GAGAL: sesi MATI saat Stop→Start ({reason})");
                    std::process::exit(1);
                }
                eprintln!("[sender] sesi berakhir: {reason}");
                std::process::exit(1);
            }
            _ => {}
        }
    }
}

async fn send_msg(sink: &mut WsSink, msg: &ClientMsg) -> Result<(), Box<dyn std::error::Error>> {
    sink.send(Message::Text(serde_json::to_string(msg)?.into()))
        .await?;
    Ok(())
}

/// Receiver: tunggu offer → answer → trickle ICE → balas ping dengan pong.
async fn run_receiver(sink: &mut WsSink, sig_rx: &mut mpsc::UnboundedReceiver<ServerMsg>) -> bool {
    let mut peer = match ReceiverPeer::new().await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[receiver] gagal buat peer: {e}");
            return false;
        }
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() > deadline {
            eprintln!("[receiver] timeout handshake");
            return false;
        }
        tokio::select! {
            msg = sig_rx.recv() => {
                match msg {
                    Some(ServerMsg::Offer { sdp }) => {
                        println!("[receiver] offer diterima");
                        match peer.set_offer(&sdp).await {
                            Ok(answer) => {
                                println!("[receiver] answer terkirim");
                                if send_msg(sink, &ClientMsg::Answer { sdp: answer }).await.is_err() { return false; }
                            }
                            Err(e) => {
                                eprintln!("[receiver] gagal answer: {e}");
                                return false;
                            }
                        }
                    }
                    Some(ServerMsg::Ice { candidate }) => {
                        if peer.add_ice(&candidate).await.is_err() { return false; }
                    }
                    Some(ServerMsg::Bye { reason }) => {
                        // `mirror-stop` = Stop yang diminta sender (T6 re-arm);
                        // receiver memang diharapkan menerima ini, bukan error.
                        if reason == "mirror-stop" {
                            println!("[receiver] bye mirror-stop (diharapkan, sender Stop)");
                            return true;
                        }
                        eprintln!("[receiver] bye dari server: {reason}");
                        return false;
                    }
                    Some(ServerMsg::Error { code, message }) => {
                        eprintln!("[receiver] error: {code:?} {message}");
                        return false;
                    }
                    None => return false,
                    _ => {}
                }
            }
            ev = peer.next_event(Duration::from_secs(5)) => {
                match ev {
                    Ok(PeerEvent::Ice(c)) => {
                        if send_msg(sink, &ClientMsg::Ice { candidate: c }).await.is_err() { return false; }
                    }
                    Ok(PeerEvent::Connected) => println!("[receiver] peer connected"),
                    Ok(PeerEvent::CtrlOpen) => {}
                    Ok(PeerEvent::CtrlMessage(m)) => {
                        if m == "ping" {
                            println!("[receiver] ping diterima → kirim pong");
                            if peer.send_ctrl("pong").await.is_err() { return false; }
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            return true;
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }
}
