//! Embedded signaling server (axum WebSocket).
//!
//! Menangani pairing token + relay SDP/ICE antara sender (WAJIB loopback)
//! dan satu receiver aktif (LAN, token wajib). Lihat
//! `docs/SIGNALING_PROTOCOL.md` untuk kontrak pesan.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::protocol::{
    ClientMsg, ErrorCode, PROTO_VERSION, ReceiverCaps, Role, ServerMsg, WS_PATH, server_ident,
};

/// Kanal keluar per-koneksi: mengantar ServerMsg ke task penulis WS.
type PeerTx = mpsc::UnboundedSender<OutBox>;

/// Pesan untuk task penulis WS: kirim JSON atau tutup dengan handshake.
enum OutBox {
    Msg(ServerMsg),
    Close,
}

#[derive(Default)]
struct Slots {
    sender: Option<PeerTx>,
    receiver: Option<PeerTx>,
    /// deviceId receiver aktif (untuk notifikasi saat sender connect belakangan).
    receiver_device_id: Option<String>,
    /// Kemampuan receiver aktif (diteruskan dari hello; None = receiver lama).
    receiver_caps: Option<ReceiverCaps>,
}

struct SharedState {
    slots: tokio::sync::Mutex<Slots>,
    /// Token pairing 6-digit aktif untuk sesi server ini.
    token: String,
}

impl SharedState {
    fn with_token(token: Option<String>) -> Self {
        Self {
            slots: tokio::sync::Mutex::new(Slots::default()),
            token: token.unwrap_or_else(|| format!("{:06}", fastrand::u32(0..1_000_000))),
        }
    }
}

/// Handle server yang sedang berjalan.
pub struct SignalingServer {
    /// Address yang di-bind (port ephemeral akurat kalau minta port 0).
    pub local_addr: SocketAddr,
    /// Token pairing aktif.
    pub token: String,
    shutdown: mpsc::Sender<()>,
}

impl SignalingServer {
    /// Hentikan server (task axum berakhir; koneksi WS ditutup OS).
    pub async fn stop(&self) {
        let _ = self.shutdown.send(()).await;
    }
}

static CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Spawn signaling server di `addr` (pakai port 0 untuk ephemeral).
///
/// Token di-generate acak per start. Untuk token yang stabil lintas restart
/// (agar TV tidak perlu ketik ulang), pakai [`spawn_with_token`].
pub async fn spawn(bind_addr: SocketAddr) -> std::io::Result<SignalingServer> {
    spawn_with_token(bind_addr, None).await
}

/// Spawn signaling server dengan token pairing tertentu.
///
/// `Some(token)` dipakai apa adanya (dipersist caller, mis. app-data);
/// `None` menghasilkan token acak 6-digit.
pub async fn spawn_with_token(
    bind_addr: SocketAddr,
    token: Option<String>,
) -> std::io::Result<SignalingServer> {
    let state = Arc::new(SharedState::with_token(token));
    let token = state.token.clone();

    let app = Router::new()
        .route(WS_PATH, get(ws_upgrade))
        .with_state(state);

    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.recv().await;
        })
        .await
        .ok();
    });

    Ok(SignalingServer {
        local_addr,
        token,
        shutdown: shutdown_tx,
    })
}

async fn ws_upgrade(
    State(state): State<Arc<SharedState>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    tracing::debug!(%peer_addr, "WS connect");
    ws.on_upgrade(move |socket| handle_socket(state, peer_addr, socket))
}

/// Kirim error lalu tutup slot — helper sinkron.
fn send_error(tx: &PeerTx, code: ErrorCode, message: &str) {
    let _ = tx.send(OutBox::Msg(ServerMsg::Error {
        code,
        message: message.to_string(),
    }));
}

/// Kirim pesan biasa.
fn send_msg(tx: &PeerTx, msg: ServerMsg) {
    let _ = tx.send(OutBox::Msg(msg));
}

/// Tutup koneksi dengan WS close handshake yang benar.
fn send_close(tx: &PeerTx) {
    let _ = tx.send(OutBox::Close);
}

async fn handle_socket(state: Arc<SharedState>, peer_addr: SocketAddr, socket: WebSocket) {
    let _conn_id = CONN_ID.fetch_add(1, Ordering::Relaxed);
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Kanal penulis: task ini satu-satunya pemilik sink WS.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutBox>();

    // Role dikunci saat hello; sebelum itu pesan ditolak.
    let mut role: Option<Role> = None;
    // Perlu kirim receiverJoined setelah helloOk (sender connect belakangan).
    let mut notify_joined = false;

    // Task penulis: ServerMsg -> WS text frame; Close -> close handshake.
    let writer = tokio::spawn(async move {
        while let Some(item) = out_rx.recv().await {
            match item {
                OutBox::Msg(msg) => {
                    let Ok(json) = serde_json::to_string(&msg) else {
                        continue;
                    };
                    if ws_sink.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                OutBox::Close => {
                    let _ = ws_sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    loop {
        let Some(Ok(frame)) = ws_stream.next().await else {
            break;
        };
        match frame {
            Message::Text(text) => {
                let Ok(msg) = serde_json::from_str::<ClientMsg>(text.as_str()) else {
                    send_error(&out_tx, ErrorCode::Unexpected, "JSON tidak valid");
                    continue;
                };
                match msg {
                    ClientMsg::Hello {
                        role: wanted,
                        proto,
                        token,
                        device_id,
                        caps,
                    } => {
                        if role.is_some() {
                            send_error(&out_tx, ErrorCode::Unexpected, "hello dua kali");
                            continue;
                        }
                        if proto != PROTO_VERSION {
                            send_error(
                                &out_tx,
                                ErrorCode::ProtoMismatch,
                                &format!("server proto {PROTO_VERSION}, client {proto}"),
                            );
                            continue;
                        }
                        match wanted {
                            Role::Sender => {
                                // Sender hanya boleh dari loopback.
                                if !peer_addr.ip().is_loopback() {
                                    send_error(
                                        &out_tx,
                                        ErrorCode::BadToken,
                                        "role sender hanya untuk koneksi loopback",
                                    );
                                    send_close(&out_tx);
                                    break;
                                }
                                let mut slots = state.slots.lock().await;
                                if slots.sender.is_some() {
                                    send_error(
                                        &out_tx,
                                        ErrorCode::SenderTaken,
                                        "slot sender sudah terisi",
                                    );
                                    send_close(&out_tx);
                                    break;
                                }
                                slots.sender = Some(out_tx.clone());
                                // Bila receiver sudah menunggu lebih dulu, sender
                                // harus diberi tahu — TAPI setelah helloOk (lihat
                                // `notify_joined` di bawah), karena sender session
                                // mengabaikan pesan selain helloOk selama handshake.
                                notify_joined = slots.receiver.is_some();
                                role = Some(Role::Sender);
                            }
                            Role::Receiver => {
                                let token = token.unwrap_or_default();
                                let mut slots = state.slots.lock().await;
                                if token != state.token {
                                    drop(slots);
                                    send_error(&out_tx, ErrorCode::BadToken, "token pairing salah");
                                    send_close(&out_tx);
                                    break;
                                }
                                // Receiver terakhir menang: koneksi baru dengan token
                                // valid MENGGANTIKAN receiver lama (yang lama diberi
                                // bye "replaced"). Tanpa ini, slot basi membuat TV
                                // tidak bisa reconnect (dead-end senderBusy).
                                if let Some(old_tx) = slots.receiver.take() {
                                    send_msg(
                                        &old_tx,
                                        ServerMsg::Bye {
                                            reason: "replaced".to_string(),
                                        },
                                    );
                                }
                                slots.receiver = Some(out_tx.clone());
                                slots.receiver_device_id = device_id.clone();
                                slots.receiver_caps = caps;
                                // Kabari sender bahwa receiver masuk.
                                let notice = ServerMsg::ReceiverJoined {
                                    device_id: device_id.clone(),
                                    caps,
                                };
                                if let Some(tx) = &slots.sender {
                                    send_msg(tx, notice);
                                }
                                role = Some(Role::Receiver);
                            }
                        }
                        send_msg(
                            &out_tx,
                            ServerMsg::HelloOk {
                                proto: PROTO_VERSION,
                                server: server_ident(),
                            },
                        );
                        // Kirim receiverJoined SETELAH helloOk (urutan penting).
                        if notify_joined {
                            let (device_id, caps) = {
                                let slots = state.slots.lock().await;
                                (slots.receiver_device_id.clone(), slots.receiver_caps)
                            };
                            send_msg(&out_tx, ServerMsg::ReceiverJoined { device_id, caps });
                        }
                    }
                    ClientMsg::Offer { sdp } => {
                        if role != Some(Role::Sender) {
                            send_error(&out_tx, ErrorCode::Unexpected, "offer hanya dari sender");
                            continue;
                        }
                        let slots = state.slots.lock().await;
                        match &slots.receiver {
                            Some(rx_tx) => {
                                send_msg(rx_tx, ServerMsg::Offer { sdp });
                            }
                            None => send_error(
                                &out_tx,
                                ErrorCode::Unexpected,
                                "receiver belum terhubung",
                            ),
                        }
                    }
                    ClientMsg::Answer { sdp } => {
                        if role != Some(Role::Receiver) {
                            send_error(
                                &out_tx,
                                ErrorCode::Unexpected,
                                "answer hanya dari receiver",
                            );
                            continue;
                        }
                        let slots = state.slots.lock().await;
                        match &slots.sender {
                            Some(tx) => {
                                send_msg(tx, ServerMsg::Answer { sdp });
                            }
                            None => {
                                send_error(&out_tx, ErrorCode::Unexpected, "sender belum terhubung")
                            }
                        }
                    }
                    ClientMsg::Ice { candidate } => {
                        let my_role = match role {
                            Some(r) => r,
                            None => {
                                send_error(&out_tx, ErrorCode::Unexpected, "hello dulu");
                                continue;
                            }
                        };
                        let slots = state.slots.lock().await;
                        let peer = match my_role {
                            Role::Sender => slots.receiver.as_ref(),
                            Role::Receiver => slots.sender.as_ref(),
                        };
                        if let Some(peer_tx) = peer {
                            send_msg(peer_tx, ServerMsg::Ice { candidate });
                        }
                    }
                    ClientMsg::SessionConfig { audio } => {
                        // Hanya sender yang boleh mengatur sesi; relay ke receiver.
                        if role != Some(Role::Sender) {
                            send_error(
                                &out_tx,
                                ErrorCode::Unexpected,
                                "sessionConfig hanya dari sender",
                            );
                            continue;
                        }
                        let slots = state.slots.lock().await;
                        match &slots.receiver {
                            Some(rx_tx) => send_msg(rx_tx, ServerMsg::SessionConfig { audio }),
                            None => send_error(
                                &out_tx,
                                ErrorCode::Unexpected,
                                "receiver belum terhubung",
                            ),
                        }
                    }
                    ClientMsg::Bye { reason } => {
                        // Forward bye ke lawan TANPA menutup koneksi pengirim.
                        // Penutupan sesi tetap lewat WS close (siapa pun yang
                        // putus). Tanpa ini, bye "mirror-stop" dari sender
                        // malah mematikan sesi WS sender sendiri (bug C1:
                        // "sesi sender mati" setelah Stop → Start).
                        let slots = state.slots.lock().await;
                        let peer = role.and_then(|r| match r {
                            Role::Sender => slots.receiver.as_ref(),
                            Role::Receiver => slots.sender.as_ref(),
                        });
                        if let Some(peer_tx) = peer {
                            send_msg(
                                peer_tx,
                                ServerMsg::Bye {
                                    reason: reason.unwrap_or_else(|| "peerBye".to_string()),
                                },
                            );
                        }
                    }
                }
            }
            Message::Ping(data) => {
                // Balas pong lewat task penulis tidak bisa (butuh binary frame);
                // kirim langsung dari sini tidak bisa karena sink di writer.
                // Solusi: kanal ping via out? Sederhana: ping kecil abaikan —
                // tungstenite client jarang ping. Untuk T3 cukup; heart-beat
                // proper menyusul di T4 (butuh un-split socket).
                let _ = data;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup slot milik koneksi ini + kabari lawan.
    let mut slots = state.slots.lock().await;
    match role {
        Some(Role::Sender) => {
            slots.sender = None;
            if let Some(rx) = &slots.receiver {
                send_msg(
                    rx,
                    ServerMsg::Bye {
                        reason: "peerDisconnected".to_string(),
                    },
                );
            }
        }
        Some(Role::Receiver) => {
            // Hanya bersihkan slot bila masih milik koneksi INI. Bila slot sudah
            // diambil alih receiver baru (last-receiver-wins), disconnect koneksi
            // lama tidak boleh menghapus slot baru atau memberi tahu sender.
            let still_mine = slots
                .receiver
                .as_ref()
                .is_some_and(|tx| tx.same_channel(&out_tx));
            if still_mine {
                slots.receiver = None;
                slots.receiver_device_id = None;
                slots.receiver_caps = None;
                if let Some(tx) = &slots.sender {
                    send_msg(
                        tx,
                        ServerMsg::Bye {
                            reason: "peerDisconnected".to_string(),
                        },
                    );
                }
            }
        }
        None => {}
    }
    drop(slots);
    // Matikan penulis dengan anggun: tutup kanal agar antrean (termasuk
    // error + close handshake) terkirim dulu; abort hanya sebagai fallback.
    drop(out_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    type TestWs = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn connect(addr: SocketAddr) -> TestWs {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{WS_PATH}"))
            .await
            .expect("connect WS");
        ws
    }

    async fn send_json<S: serde::Serialize>(ws: &mut TestWs, v: &S) {
        ws.send(WsMessage::Text(serde_json::to_string(v).unwrap().into()))
            .await
            .unwrap();
    }

    async fn next_server_msg<R: serde::de::DeserializeOwned>(ws: &mut TestWs) -> R {
        loop {
            let msg = ws.next().await.unwrap().unwrap();
            if let WsMessage::Text(t) = msg {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn sender_connecting_after_receiver_is_notified() {
        // Regresi: sender yang connect belakangan harus tetap dapat
        // receiverJoined, kalau tidak ia tak akan pernah mengirim offer.
        let server = spawn(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();
        let addr = server.local_addr;

        // Receiver connect lebih dulu (belum ada sender).
        let mut receiver = connect(addr).await;
        send_json(
            &mut receiver,
            &ClientMsg::Hello {
                role: Role::Receiver,
                proto: PROTO_VERSION,
                token: Some(server.token.clone()),
                device_id: Some("early-tv".into()),
                caps: None,
            },
        )
        .await;
        let ok: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(ok, ServerMsg::HelloOk { .. }));

        // Sender baru connect setelahnya.
        let mut sender = connect(addr).await;
        send_json(
            &mut sender,
            &ClientMsg::Hello {
                role: Role::Sender,
                proto: PROTO_VERSION,
                token: None,
                device_id: None,
                caps: None,
            },
        )
        .await;
        let ok: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(ok, ServerMsg::HelloOk { .. }));

        // Harus langsung dapat receiverJoined (bukan menunggu event lain).
        let joined: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(
            joined,
            ServerMsg::ReceiverJoined { ref device_id, .. }
            if device_id.as_deref() == Some("early-tv")
        ));

        server.stop().await;
    }

    #[tokio::test]
    async fn hello_relay_and_errors() {
        let server = spawn(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();
        let addr = server.local_addr;

        // --- sender hello (loopback) -> helloOk
        let mut sender = connect(addr).await;
        send_json(
            &mut sender,
            &ClientMsg::Hello {
                role: Role::Sender,
                proto: PROTO_VERSION,
                token: None,
                device_id: None,
                caps: None,
            },
        )
        .await;
        let ok: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(ok, ServerMsg::HelloOk { proto: 1, .. }));

        // --- receiver hello token salah -> error badToken
        let mut bad = connect(addr).await;
        send_json(
            &mut bad,
            &ClientMsg::Hello {
                role: Role::Receiver,
                proto: PROTO_VERSION,
                token: Some("000000".to_string()),
                device_id: None,
                caps: None,
            },
        )
        .await;
        let err: ServerMsg = next_server_msg(&mut bad).await;
        assert!(matches!(
            err,
            ServerMsg::Error {
                code: ErrorCode::BadToken,
                ..
            }
        ));

        // --- receiver hello token benar -> helloOk
        let mut receiver = connect(addr).await;
        send_json(
            &mut receiver,
            &ClientMsg::Hello {
                role: Role::Receiver,
                proto: PROTO_VERSION,
                token: Some(server.token.clone()),
                device_id: Some("test-tv".into()),
                caps: None,
            },
        )
        .await;
        let ok: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(ok, ServerMsg::HelloOk { .. }));

        // Sender mendapat notifikasi receiverJoined.
        let joined: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(
            joined,
            ServerMsg::ReceiverJoined { ref device_id, .. }
            if device_id.as_deref() == Some("test-tv")
        ));

        // --- receiver kedua (token sama) MENGGANTIKAN receiver pertama:
        // receiver lama dapat bye "replaced", yang baru helloOk.
        let mut second = connect(addr).await;
        send_json(
            &mut second,
            &ClientMsg::Hello {
                role: Role::Receiver,
                proto: PROTO_VERSION,
                token: Some(server.token.clone()),
                device_id: Some("test-tv-2".into()),
                caps: None,
            },
        )
        .await;
        let ok2: ServerMsg = next_server_msg(&mut second).await;
        assert!(matches!(ok2, ServerMsg::HelloOk { .. }));
        let replaced: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(
            replaced,
            ServerMsg::Bye { ref reason, .. } if reason == "replaced"
        ));
        // Sender diberi tahu receiver baru masuk.
        let joined2: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(
            joined2,
            ServerMsg::ReceiverJoined { ref device_id, .. }
            if device_id.as_deref() == Some("test-tv-2")
        ));
        // Receiver lama sudah diganti: gunakan koneksi `second` selanjutnya.
        receiver = second;

        // --- offer dari sender sampai ke receiver
        send_json(
            &mut sender,
            &ClientMsg::Offer {
                sdp: "v=0 OFFER-TEST".to_string(),
            },
        )
        .await;
        let off: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(off, ServerMsg::Offer { ref sdp } if sdp == "v=0 OFFER-TEST"));

        // --- answer dari receiver sampai ke sender
        send_json(
            &mut receiver,
            &ClientMsg::Answer {
                sdp: "v=0 ANSWER-TEST".to_string(),
            },
        )
        .await;
        let ans: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(ans, ServerMsg::Answer { ref sdp } if sdp == "v=0 ANSWER-TEST"));

        // --- ice dua arah
        let cand = super::super::protocol::IceCandidate {
            candidate: "candidate:1 udp 1 10.0.0.1 9999 typ host".into(),
            sdp_mid: "0".into(),
            sdp_mline_index: 0,
        };
        send_json(
            &mut sender,
            &ClientMsg::Ice {
                candidate: cand.clone(),
            },
        )
        .await;
        let ice: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(ice, ServerMsg::Ice { .. }));

        // --- offer sebelum receiver (role salah) ditolak
        send_json(
            &mut receiver,
            &ClientMsg::Offer {
                sdp: "v=0 X".to_string(),
            },
        )
        .await;
        let err: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(
            err,
            ServerMsg::Error {
                code: ErrorCode::Unexpected,
                ..
            }
        ));

        // --- receiver disconnect -> sender dapat bye peerDisconnected
        drop(receiver);
        let bye: ServerMsg = next_server_msg(&mut sender).await;
        assert!(matches!(
            bye,
            ServerMsg::Bye {
                ref reason,
                ..
            } if reason == "peerDisconnected"
        ));

        server.stop().await;
    }

    /// caps receiver diteruskan ke sender lewat receiverJoined.
    #[tokio::test]
    async fn receiver_caps_are_relayed_to_sender() {
        use super::super::protocol::AudioRoute;
        let server = spawn(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();
        let addr = server.local_addr;

        let mut sender = connect(addr).await;
        send_json(
            &mut sender,
            &ClientMsg::Hello {
                role: Role::Sender,
                proto: PROTO_VERSION,
                token: None,
                device_id: None,
                caps: None,
            },
        )
        .await;
        let _: ServerMsg = next_server_msg(&mut sender).await; // helloOk

        let mut receiver = connect(addr).await;
        send_json(
            &mut receiver,
            &ClientMsg::Hello {
                role: Role::Receiver,
                proto: PROTO_VERSION,
                token: Some(server.token.clone()),
                device_id: Some("tv-audio".into()),
                caps: Some(ReceiverCaps { audio: true }),
            },
        )
        .await;
        let ok: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(ok, ServerMsg::HelloOk { .. }));

        let joined: ServerMsg = next_server_msg(&mut sender).await;
        match joined {
            ServerMsg::ReceiverJoined { device_id, caps } => {
                assert_eq!(device_id.as_deref(), Some("tv-audio"));
                assert_eq!(caps, Some(ReceiverCaps { audio: true }));
            }
            other => panic!("harus receiverJoined: {other:?}"),
        }

        // sessionConfig dari sender sampai ke receiver.
        let cfg = super::super::protocol::AudioSessionConfig::for_route(AudioRoute::Both);
        send_json(
            &mut sender,
            &ClientMsg::SessionConfig { audio: cfg.clone() },
        )
        .await;
        let relayed: ServerMsg = next_server_msg(&mut receiver).await;
        match relayed {
            ServerMsg::SessionConfig { audio } => assert_eq!(audio, cfg),
            other => panic!("harus sessionConfig: {other:?}"),
        }

        // Receiver TIDAK boleh mengirim sessionConfig.
        send_json(
            &mut receiver,
            &ClientMsg::SessionConfig {
                audio: super::super::protocol::AudioSessionConfig::for_route(AudioRoute::Muted),
            },
        )
        .await;
        let err: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(
            err,
            ServerMsg::Error {
                code: ErrorCode::Unexpected,
                ..
            }
        ));

        server.stop().await;
    }

    /// Receiver lama (tanpa caps) tetap bekerja: receiverJoined tanpa caps.
    #[tokio::test]
    async fn legacy_receiver_without_caps_still_joins() {
        let server = spawn(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();
        let addr = server.local_addr;

        let mut sender = connect(addr).await;
        send_json(
            &mut sender,
            &ClientMsg::Hello {
                role: Role::Sender,
                proto: PROTO_VERSION,
                token: None,
                device_id: None,
                caps: None,
            },
        )
        .await;
        let _: ServerMsg = next_server_msg(&mut sender).await;

        // Hello receiver lama (tanpa caps) — JSON mentah agar jelas absen.
        let hello = format!(
            r#"{{"type":"hello","role":"receiver","proto":1,"token":"{}","deviceId":"old-tv"}}"#,
            server.token
        );
        let mut receiver = connect(addr).await;
        receiver.send(WsMessage::Text(hello.into())).await.unwrap();
        let ok: ServerMsg = next_server_msg(&mut receiver).await;
        assert!(matches!(ok, ServerMsg::HelloOk { .. }));

        let joined: ServerMsg = next_server_msg(&mut sender).await;
        match joined {
            ServerMsg::ReceiverJoined { device_id, caps } => {
                assert_eq!(device_id.as_deref(), Some("old-tv"));
                assert_eq!(caps, None, "receiver lama tidak punya caps");
            }
            other => panic!("harus receiverJoined: {other:?}"),
        }

        server.stop().await;
    }
}
