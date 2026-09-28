//! Sesi sender yang reusable: WS client role `sender` + PeerConnection.
//!
//! Dipakai src-tauri (T4) dan example test client T3 — satu implementasi
//! handshake agar perilaku teruji identik. Berjalan sebagai background
//! task; API berupa command channel masuk dan event channel keluar.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use tokio::sync::watch;

use super::peer::{PeerEvent, SenderPeer};
use super::protocol::{
    AudioRoute, AudioSessionConfig, ClientMsg, PROTO_VERSION, ReceiverCaps, Role, ServerMsg,
};
use crate::audio as audio_mod;
use crate::audio::pipeline::{self as audio_pipeline, AudioPipelineStats};
use crate::capture::ScreenCapturer;
use crate::encode::{EncoderConfig, H264Tuning};
use crate::stream::{self, PipelineConfig, PipelineStats, TrackSink};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;

/// Bentuk sesi sender.
///
/// Jalur video produksi (T4/T6) memakai [`SenderMode::VideoOnly`]: offer
/// hanya berisi m=video, yang terbukti interoperable dengan libwebrtc
/// Android (m=application membuat negosiasi gagal).
/// [`SenderMode::WithCtrl`] menambah DataChannel `ctrl` (dipakai test T3:
/// ping/pong).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderMode {
    VideoOnly,
    WithCtrl,
}

fn h264_ab_settings_from_env() -> Result<H264Tuning, String> {
    let profile = std::env::var("WDT_H264_PROFILE").ok();
    let cabac = std::env::var("WDT_H264_CABAC").ok();
    H264Tuning::parse(profile.as_deref(), cabac.as_deref())
}

/// Sumber gambar untuk satu sesi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Display fisik/aktif yang sudah ada (ID dari `available_displays`).
    Display(String),
    /// Display virtual (Extended) — dibuat saat pipeline mulai, dilepas saat
    /// berhenti. Eksperimental; lihat `crate::vdisplay`.
    Virtual {
        width: u32,
        height: u32,
        refresh_hz: u32,
    },
}

/// Resolusi target stream (diakhiri ke rata-atas genap oleh pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionPreset {
    /// 1080p (default; kanvas 1920×1080, TV menampilkan 1:1).
    P1080,
    /// 720p (hemat CPU/jaringan untuk TV low-end).
    P720,
}

impl ResolutionPreset {
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            ResolutionPreset::P1080 => (1920, 1080),
            ResolutionPreset::P720 => (1280, 720),
        }
    }
}

/// Preset kualitas encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityPreset {
    /// Konstanta kualitas 0.9, cap 10 Mbps (perilaku R3–R6).
    Balanced,
    /// Ketajaman teks: kualitas 0.95, cap 18 Mbps (LAN stabil).
    Sharp,
}

impl QualityPreset {
    pub fn quality(self) -> f32 {
        match self {
            QualityPreset::Balanced => 0.9,
            QualityPreset::Sharp => 0.95,
        }
    }

    /// Cap bitrate (bit/s) — menjadi `bitrate_bps` pipeline.
    pub fn bitrate_cap_bps(self) -> u32 {
        match self {
            QualityPreset::Balanced => 10_000_000,
            QualityPreset::Sharp => 18_000_000,
        }
    }
}

/// Pengaturan video user-facing untuk satu sesi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSettings {
    pub resolution: ResolutionPreset,
    /// 30 atau 60 fps.
    pub fps: u32,
    pub quality: QualityPreset,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            resolution: ResolutionPreset::P1080,
            fps: 30,
            quality: QualityPreset::Balanced,
        }
    }
}

/// Perintah ke sesi sender.
#[derive(Debug)]
pub enum SessionCmd {
    /// Buat offer dan kirim ke receiver aktif. `target` memilih display fisik
    /// atau virtual; `audio_route` menentukan pengiriman sample audio;
    /// `video` mengatur resolusi/fps/kualitas pipeline.
    StartOffer {
        target: CaptureTarget,
        audio_route: AudioRoute,
        video: VideoSettings,
    },
    /// Ubah tujuan suara saat sesi aktif (live switching, tanpa renegosiasi).
    SetAudioRoute(AudioRoute),
    /// Kirim teks via DataChannel `ctrl`.
    SendCtrl(String),
    /// Berhenti mirror (pipeline + peer direset, kirim bye) tetapi **sesi WS
    /// tetap hidup** sehingga Start bisa ditekan lagi tanpa restart app.
    StopMirroring,
    /// Tutup seluruh sesi secara sopan (kirim bye lalu keluar).
    Shutdown,
}

/// Event keluar dari sesi sender untuk UI/logging.
#[derive(Debug)]
pub enum SessionEvent {
    /// Hello diterima server.
    HelloOk { server: String },
    /// Receiver baru terhubung (untuk daftar TV di UI).
    ReceiverJoined {
        device_id: Option<String>,
        device_name: Option<String>,
        /// Kemampuan receiver (None = receiver lama tanpa audio).
        caps: Option<super::protocol::ReceiverCaps>,
    },
    /// Offer terkirim ke receiver.
    OfferSent,
    /// Answer diterima dari receiver.
    AnswerReceived,
    /// PeerConnection Connected.
    PeerConnected,
    /// DataChannel `ctrl` terbuka.
    CtrlOpen,
    /// Pesan teks masuk dari receiver.
    CtrlMessage(String),
    /// Sesi berakhir (Stop, WS putus, atau error fatal).
    Ended { reason: String },
    /// Receiver lawan disconnect (sesi tetap hidup menunggu receiver baru).
    ReceiverLeft { reason: String },
    /// Pipeline mulai: resolusi/fps + PT/SSRC ternegosiasi (untuk diagnosa
    /// interop — PT harus sama dengan yang diiklankan di SDP).
    PipelineStarted {
        width: u32,
        height: u32,
        fps: u32,
        ssrc: u32,
        payload_type: u8,
    },
    /// Statistik pipeline (~tiap 2 dtk) untuk UI/logging.
    PipelineStats(PipelineStats),
    /// Statistik pipeline audio (~tiap 2 dtk).
    AudioStats(AudioPipelineStats),
    /// Route audio aktif berubah (dikonfirmasi ke UI) — live switching.
    AudioRouteChanged { route: AudioRoute },
    /// Audio bermasalah (non-fatal; video tetap jalan). Mis. izin dicabut,
    /// device berubah, atau receiver tidak mendukung audio.
    AudioDegraded { message: String },
    /// Mirroring berhenti (perintah StopMirroring) — sesi WS masih hidup.
    MirroringStopped,
    /// Error fatal sesi.
    Error(String),
}

/// Error pembuatan/koneksi awal sesi.
#[derive(Debug)]
pub struct SessionError(pub String);

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sender session: {}", self.0)
    }
}

impl std::error::Error for SessionError {}

/// Handle sesi sender yang berjalan di background.
pub struct SenderSession {
    /// Kirim perintah ke task sesi.
    pub cmds: mpsc::UnboundedSender<SessionCmd>,
    /// Terima event dari task sesi (poll oleh pemilik).
    pub events: mpsc::UnboundedReceiver<SessionEvent>,
}

/// Connect sebagai sender ke `url`, handshake hello, lalu jalankan
/// background task. Mengembalikan handle segera setelah helloOk.
pub async fn spawn_sender_session(
    url: &str,
    mode: SenderMode,
) -> Result<SenderSession, SessionError> {
    let h264 = h264_ab_settings_from_env().map_err(SessionError)?;
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| SessionError(format!("connect WS gagal: {e}")))?;
    let (mut sink, mut stream) = ws.split();

    send_msg(
        &mut sink,
        &ClientMsg::Hello {
            role: Role::Sender,
            proto: PROTO_VERSION,
            token: None,
            device_id: None,
            device_name: None,
            caps: None,
        },
    )
    .await?;
    let server = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<ServerMsg>(t.as_str()) {
                    Ok(ServerMsg::HelloOk { server, .. }) => return Ok(server),
                    Ok(ServerMsg::Error { code, message }) => {
                        return Err(format!("hello ditolak: {code:?} {message}"));
                    }
                    Ok(_) => continue,
                    Err(e) => return Err(format!("pesan tak terduga: {e}")),
                },
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(format!("WS error: {e}")),
                None => return Err("WS tertutup sebelum helloOk".to_string()),
            }
        }
    })
    .await
    .map_err(|_| SessionError("timeout menunggu helloOk".to_string()))?
    .map_err(SessionError)?;

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let _ = ev_tx.send(SessionEvent::HelloOk { server });
    tokio::spawn(drive(sink, stream, cmd_rx, ev_tx, mode, h264));

    Ok(SenderSession {
        cmds: cmd_tx,
        events: ev_rx,
    })
}

async fn send_msg(sink: &mut WsSink, msg: &ClientMsg) -> Result<(), SessionError> {
    sink.send(Message::Text(
        serde_json::to_string(msg)
            .map_err(|e| SessionError(format!("encode JSON: {e}")))?
            .into(),
    ))
    .await
    .map_err(|e| SessionError(format!("kirim WS gagal: {e}")))
}

async fn drive(
    mut sink: WsSink,
    mut stream: futures_util::stream::SplitStream<WsStream>,
    mut cmds: mpsc::UnboundedReceiver<SessionCmd>,
    events: mpsc::UnboundedSender<SessionEvent>,
    mode: SenderMode,
    h264: H264Tuning,
) {
    let mut peer: Option<SenderPeer> = None;
    let mut pipeline: Option<PipelineHandle> = None;
    let mut audio: Option<AudioPipelineHandle> = None;
    let mut selected_target: Option<CaptureTarget> = None;
    let mut video_settings = VideoSettings::default();
    let mut audio_route: AudioRoute = AudioRoute::Laptop;
    // Kemampuan receiver aktif (None = receiver lama / belum terhubung).
    let mut receiver_caps: Option<ReceiverCaps> = None;
    // Tick pompa agar event peer (ICE trickle) diteruskan segera walau
    // tidak ada aktivitas WS/command.
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = tick.tick() => {
                match drain_peer(&mut sink, peer.as_mut(), &events).await {
                    Ok(true) if pipeline.is_none() => {
                        // Peer baru connected → mulai pipeline capture/encode.
                        match maybe_start_pipeline(
                            &mut pipeline,
                            peer.as_ref(),
                            selected_target.as_ref(),
                            video_settings,
                            &events,
                            h264,
                        ).await {
                            Ok(()) => {}
                            Err(e) => {
                                let _ = events.send(SessionEvent::Error(format!("pipeline: {e}")));
                            }
                        }
                        start_audio_if_supported(&mut audio, peer.as_ref(), audio_route, &events).await;
                    }
                    Ok(_) => {}
                    Err(()) => break,
                }
            }
            cmd = cmds.recv() => {
                match cmd {
                    Some(SessionCmd::StartOffer { target, audio_route: route, video }) => {
                        if peer.is_some() {
                            let _ = events.send(SessionEvent::Error("offer sudah berjalan".into()));
                            continue;
                        }
                        audio_route = route;
                        // Bila route minta audio ke TV tetapi receiver tidak
                        // mendukung, beri tahu UI secara jujur (video tetap).
                        if route.sends_to_tv() && receiver_caps.map(|c| c.audio) != Some(true) {
                            let _ = events.send(SessionEvent::AudioDegraded {
                                message: "TV ini belum mendukung audio. Video tetap dikirim."
                                    .to_string(),
                            });
                            audio_route = AudioRoute::Laptop;
                        }
                        let p = match match mode {
                            SenderMode::VideoOnly => {
                                // Receiver mendukung audio → negosiasikan m=audio
                                // sejak awal (pre-negotiated) agar live switch TV↔
                                // Laptop tidak perlu renegosiasi.
                                if receiver_caps.map(|c| c.audio) == Some(true) {
                                    SenderPeer::av_with_profile(h264.profile).await
                                } else {
                                    SenderPeer::video_only_with_profile(h264.profile).await
                                }
                            }
                            SenderMode::WithCtrl => {
                                SenderPeer::with_ctrl_profile(h264.profile).await
                            }
                        } {
                            Ok(p) => p,
                            Err(e) => {
                                let _ = events.send(SessionEvent::Error(format!("buat peer: {e}")));
                                continue;
                            }
                        };
                        let offer = match p.create_offer().await {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = events.send(SessionEvent::Error(format!("buat offer: {e}")));
                                continue;
                            }
                        };
                        if send_msg(&mut sink, &ClientMsg::Offer { sdp: offer }).await.is_err() {
                            let _ = events.send(SessionEvent::Ended { reason: "ws-putus".into() });
                            break;
                        }
                        let _ = events.send(SessionEvent::OfferSent);
                        // Beri tahu receiver konfigurasi audio awal (sebelum
                        // answer) agar ia tidak mengambil audio focus bila
                        // route belum aktif.
                        let _ = send_msg(
                            &mut sink,
                            &ClientMsg::SessionConfig {
                                audio: AudioSessionConfig::for_route(audio_route),
                            },
                        ).await;
                        selected_target = Some(target);
                        video_settings = video;
                        peer = Some(p);
                    }
                    Some(SessionCmd::SetAudioRoute(route)) => {
                        let allowed = !route.sends_to_tv()
                            || receiver_caps.map(|c| c.audio) == Some(true);
                        if !allowed {
                            let _ = events.send(SessionEvent::AudioDegraded {
                                message: "TV ini belum mendukung audio.".to_string(),
                            });
                            continue;
                        }
                        audio_route = route;
                        // Kirim konfigurasi baru ke receiver + ubah pipeline
                        // audio yang sudah berjalan (tanpa menyentuh video).
                        if peer.is_some() {
                            let _ = send_msg(
                                &mut sink,
                                &ClientMsg::SessionConfig {
                                    audio: AudioSessionConfig::for_route(route),
                                },
                            ).await;
                        }
                        let _ = events.send(SessionEvent::AudioRouteChanged { route });
                        start_audio_if_supported(&mut audio, peer.as_ref(), route, &events).await;
                        if let Some(a) = &audio {
                            let _ = a.active.send(route.sends_to_tv());
                        }
                    }
                    Some(SessionCmd::SendCtrl(text)) => {
                        if let Some(p) = peer.as_ref()
                            && p.send_ctrl(&text).await.is_err()
                        {
                            let _ = events.send(SessionEvent::Error("kirim ctrl gagal".into()));
                        }
                    }
                    Some(SessionCmd::StopMirroring) => {
                        // Hentikan streaming + reset peer. Sesi WS & slot receiver
                        // TETAP hidup: receiver hanya re-arm (lihat reason
                        // "mirror-stop" di SIGNALING_PROTOCOL.md) sehingga Start
                        // berikutnya tidak perlu Connect ulang di TV.
                        stop_pipeline(&mut pipeline).await;
                        stop_audio_pipeline(&mut audio).await;
                        let _ = send_msg(&mut sink, &ClientMsg::Bye { reason: Some("mirror-stop".into()) }).await;
                        peer = None;
                        selected_target = None;
                        let _ = events.send(SessionEvent::MirroringStopped);
                    }
                    Some(SessionCmd::Shutdown) | None => {
                        stop_pipeline(&mut pipeline).await;
                        stop_audio_pipeline(&mut audio).await;
                        let _ = send_msg(&mut sink, &ClientMsg::Bye { reason: Some("user-stop".into()) }).await;
                        let _ = events.send(SessionEvent::Ended { reason: "user-stop".into() });
                        break;
                    }
                }
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        let parsed: ServerMsg = match serde_json::from_str(t.as_str()) {
                            Ok(m) => m,
                            Err(_) => continue,
                        };
                        match parsed {
                            ServerMsg::ReceiverJoined { device_id, device_name, caps } => {
                                receiver_caps = caps;
                                let _ = events.send(SessionEvent::ReceiverJoined {
                                    device_id,
                                    device_name,
                                    caps,
                                });
                            }
                            ServerMsg::Answer { sdp } => {
                                if let Some(p) = peer.as_ref() {
                                    match p.set_answer(&sdp).await {
                                        Ok(()) => { let _ = events.send(SessionEvent::AnswerReceived); }
                                        Err(e) => { let _ = events.send(SessionEvent::Error(format!("set answer: {e}"))); }
                                    }
                                }
                            }
                            ServerMsg::Ice { candidate } => {
                                if let Some(p) = peer.as_ref()
                                    && p.add_ice(&candidate).await.is_err()
                                {
                                    let _ = events.send(SessionEvent::Error("add ice gagal".into()));
                                }
                            }
                            ServerMsg::Bye { reason } => {
                                // Receiver pergi — hentikan streaming, sesi tetap
                                // hidup; peer lama dibuang agar StartOffer
                                // berikutnya fresh.
                                stop_pipeline(&mut pipeline).await;
                                stop_audio_pipeline(&mut audio).await;
                                peer = None;
                                selected_target = None;
                                receiver_caps = None;
                                let _ = events.send(SessionEvent::ReceiverLeft { reason });
                            }
                            ServerMsg::Error { code, message } => {
                                if is_fatal_error(&code) {
                                    let _ = events.send(SessionEvent::Error(format!("{code:?}: {message}")));
                                    break;
                                }
                                // Recoverable (mis. "receiver belum terhubung" saat
                                // Start terlalu cepat): sesi tetap hidup agar bisa
                                // diulang. Sebelumnya `break` di sini yang membuat
                                // Stop→Start gagal permanen ("sesi sender mati").
                                stop_pipeline(&mut pipeline).await;
                                stop_audio_pipeline(&mut audio).await;
                                peer = None;
                                selected_target = None;
                                tracing::warn!("error server recoverable: {code:?}: {message}");
                                let _ = events.send(SessionEvent::MirroringStopped);
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        let _ = events.send(SessionEvent::Ended { reason: "ws-tertutup".into() });
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Pompa event peer (non-blocking) bila peer sudah ada.
        match drain_peer(&mut sink, peer.as_mut(), &events).await {
            Ok(true) if pipeline.is_none() => {
                match maybe_start_pipeline(
                    &mut pipeline,
                    peer.as_ref(),
                    selected_target.as_ref(),
                    video_settings,
                    &events,
                    h264,
                )
                .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        let _ = events.send(SessionEvent::Error(format!("pipeline: {e}")));
                    }
                }
                start_audio_if_supported(&mut audio, peer.as_ref(), audio_route, &events).await;
            }
            Ok(_) => {}
            Err(()) => break,
        }
    }

    // Pastikan task pipeline tidak tertinggal saat sesi berakhir.
    stop_pipeline(&mut pipeline).await;
    stop_audio_pipeline(&mut audio).await;
}

/// Handle pipeline audio yang sedang berjalan (thread dedikasi).
struct AudioPipelineHandle {
    /// Route aktif/tidak (TV/Both vs Laptop/Muted) — live switch.
    active: watch::Sender<bool>,
    stop: watch::Sender<bool>,
    join: std::thread::JoinHandle<()>,
}

/// Mulai pipeline audio bila peer punya audio track (receiver R4).
async fn start_audio_if_supported(
    audio: &mut Option<AudioPipelineHandle>,
    peer: Option<&SenderPeer>,
    route: AudioRoute,
    events: &mpsc::UnboundedSender<SessionEvent>,
) {
    if audio.is_some() {
        return;
    }
    let Some(peer) = peer else { return };
    let Some(track) = peer.audio_track() else {
        return;
    };
    let Some((ssrc, payload_type)) = peer.audio_send_params().await else {
        let _ = events.send(SessionEvent::AudioDegraded {
            message: "Audio belum ternegosiasi dengan TV.".to_string(),
        });
        return;
    };
    match start_audio_pipeline(track, ssrc, payload_type, route, events) {
        Ok(handle) => *audio = Some(handle),
        Err(e) => {
            let _ = events.send(SessionEvent::AudioDegraded {
                message: format!("Pipeline audio gagal dimulai: {e}"),
            });
        }
    }
}

/// Jalankan pipeline audio di thread dedikasi (capturer native tidak `Send`).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn start_audio_pipeline(
    track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
    ssrc: rtc::rtp_transceiver::SSRC,
    payload_type: rtc::rtp_transceiver::PayloadType,
    route: AudioRoute,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Result<AudioPipelineHandle, String> {
    let sink = TrackSink::new(track, ssrc, payload_type);
    let (active_tx, active_rx) = watch::channel(route.sends_to_tv());
    let (stop_tx, stop_rx) = watch::channel(false);
    let ev = events.clone();

    let join = std::thread::Builder::new()
        .name("wdt-audio".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ev.send(SessionEvent::AudioDegraded {
                        message: format!("runtime audio: {e}"),
                    });
                    return;
                }
            };
            rt.block_on(async move {
                let stats_ev = ev.clone();
                let result = audio_pipeline::run(
                    audio_mod::default_capturer,
                    sink,
                    audio_pipeline::AudioPipelineConfig::default(),
                    active_rx,
                    stop_rx,
                    move |st: &AudioPipelineStats| {
                        let _ = stats_ev.send(SessionEvent::AudioStats(st.clone()));
                    },
                )
                .await;
                match result {
                    Ok(st) => tracing::info!(
                        packets = st.packets_sent,
                        sent_ms = st.sent_ms,
                        errors = st.errors,
                        "audio pipeline selesai"
                    ),
                    Err(e) => {
                        let _ = ev.send(SessionEvent::AudioDegraded {
                            message: format!("audio pipeline berhenti: {e}"),
                        });
                    }
                }
            });
        })
        .map_err(|e| format!("spawn thread audio: {e}"))?;

    Ok(AudioPipelineHandle {
        active: active_tx,
        stop: stop_tx,
        join,
    })
}

/// Platform tanpa capture audio sistem: mulai tidak mungkin (stub agar
/// `sender_session` tetap kompilasi lintas platform).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn start_audio_pipeline(
    _track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
    _ssrc: rtc::rtp_transceiver::SSRC,
    _payload_type: rtc::rtp_transceiver::PayloadType,
    _route: AudioRoute,
    _events: &mpsc::UnboundedSender<SessionEvent>,
) -> Result<AudioPipelineHandle, String> {
    Err("capture audio sistem tidak didukung di platform ini".to_string())
}

/// Hentikan pipeline audio (idempoten) dan tunggu thread berakhir.
async fn stop_audio_pipeline(audio: &mut Option<AudioPipelineHandle>) {
    if let Some(h) = audio.take() {
        let _ = h.stop.send(true);
        let join = h.join;
        let res = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::task::spawn_blocking(move || {
                let _ = join.join();
            }),
        )
        .await;
        if res.is_err() {
            tracing::warn!("audio pipeline lambat berhenti (timeout)");
        }
    }
}

/// Handle pipeline streaming yang sedang berjalan (thread dedikasi).
struct PipelineHandle {
    stop: watch::Sender<bool>,
    join: std::thread::JoinHandle<()>,
}

/// Mulai pipeline capture → encode → track untuk `peer` yang sudah connected.
///
/// Pipeline berjalan di **thread dedikasi**: `ScreenCapturer`/`FrameEncoder`
/// native (VideoToolbox, Media Foundation COM) tidak `Send`, jadi keduanya
/// dibuat dan dipakai di thread itu. Sink sample (`TrackSink`) tetap `Send`
/// sehingga bisa menulis ke track WebRTC dari thread tersebut.
fn start_pipeline(
    peer: &SenderPeer,
    target: &CaptureTarget,
    video: VideoSettings,
    ssrc: rtc::rtp_transceiver::SSRC,
    payload_type: rtc::rtp_transceiver::PayloadType,
    events: &mpsc::UnboundedSender<SessionEvent>,
    h264: H264Tuning,
) -> Result<PipelineHandle, String> {
    let sink = TrackSink::new(peer.video_track(), ssrc, payload_type);
    let (stop_tx, stop_rx) = watch::channel(false);
    let ev = events.clone();
    let target = target.clone();
    let video = video;

    let join = std::thread::Builder::new()
        .name("wdt-stream".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ev.send(SessionEvent::Error(format!("runtime pipeline: {e}")));
                    return;
                }
            };
            // Display virtual (Extended) dibuat di thread ini agar objek ObjC
            // hidup selama pipeline dan dilepas saat thread berakhir.
            // (Hanya macOS: tipe `VirtualDisplay` tidak ada di platform lain.)
            #[cfg(target_os = "macos")]
            let mut virtual_display: Option<crate::vdisplay::VirtualDisplay> = None;
            let display_id: String = match &target {
                CaptureTarget::Display(id) => id.clone(),
                CaptureTarget::Virtual {
                    width,
                    height,
                    refresh_hz,
                } => {
                    #[cfg(target_os = "macos")]
                    {
                        match crate::vdisplay::VirtualDisplay::create(*width, *height, *refresh_hz)
                        {
                            Ok(vd) => {
                                let cid = vd.capture_id();
                                virtual_display = Some(vd);
                                cid
                            }
                            Err(e) => {
                                let _ =
                                    ev.send(SessionEvent::Error(format!("layar tambahan: {e}")));
                                return;
                            }
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = (width, height, refresh_hz);
                        let _ = ev.send(SessionEvent::Error(
                            "Layar tambahan belum tersedia di platform ini".to_string(),
                        ));
                        return;
                    }
                }
            };
            rt.block_on(async move {
                // Hidupkan display virtual sampai pipeline selesai.
                #[cfg(target_os = "macos")]
                let _keep_virtual = &virtual_display;
                let capturer = match crate::capture::capturer_for_display(&display_id) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ev.send(SessionEvent::Error(format!("capturer: {e}")));
                        return;
                    }
                };
                // Q1 (T6): compose ke resolusi target (default 1920x1080 =
                // panel TV umum) sehingga TV menampilkan 1:1 TANPA scaling.
                // Konten laptop (mis. 16:10) di-letterbox proporsional oleh
                // sender. Tidak ada upscale (lihat scale_letterbox_bgra).
                let _ = capturer.display_size();
                let (mut width, mut height) = video.resolution.dimensions();
                // Override target untuk pengujian (mis. A/B latency resolusi):
                //   WDT_TARGET=1280x720 <binary>
                if let Ok(t) = std::env::var("WDT_TARGET")
                    && let Some((w, h)) = t.split_once('x')
                    && let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>())
                    && w >= 4
                    && h >= 4
                {
                    width = w & !1;
                    height = h & !1;
                    tracing::info!("WDT_TARGET override → {width}x{height}");
                }
                let base = PipelineConfig::hd720p30();
                // Override fps untuk pengujian: WDT_FPS=15 <binary>
                let fps = std::env::var("WDT_FPS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .filter(|f| *f >= 1 && *f <= 120)
                    .unwrap_or(video.fps.clamp(1, 120));
                let cfg = PipelineConfig {
                    width,
                    height,
                    bitrate_bps: video.quality.bitrate_cap_bps(),
                    fps,
                    ..base
                };
                if cfg.width == 0 || cfg.height == 0 {
                    let _ = ev.send(SessionEvent::Error("ukuran target 0".into()));
                    return;
                }
                let _ = ev.send(SessionEvent::PipelineStarted {
                    width: cfg.width,
                    height: cfg.height,
                    fps: cfg.fps,
                    ssrc,
                    payload_type,
                });

                tracing::info!(
                    profile = ?h264.profile,
                    entropy = ?h264.entropy_mode,
                    "tuning H.264 A/B aktif"
                );
                let encoder = match crate::encode::default_encoder(EncoderConfig {
                    width: cfg.width,
                    height: cfg.height,
                    bitrate_bps: cfg.bitrate_bps,
                    fps: cfg.fps,
                    keyframe_interval: cfg.keyframe_interval,
                    profile: h264.profile,
                    entropy_mode: Some(h264.entropy_mode),
                    // T6: constant quality → teks/desktop lebih tajam.
                    // Preset: Seimbang 0.9 · Tajam (teks) 0.95.
                    quality: Some(video.quality.quality()),
                }) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = ev.send(SessionEvent::Error(format!("encoder: {e}")));
                        return;
                    }
                };

                let stats_ev = ev.clone();
                match stream::run(capturer, encoder, sink, cfg, stop_rx, move |st| {
                    let _ = stats_ev.send(SessionEvent::PipelineStats(st));
                })
                .await
                {
                    Ok(st) => tracing::info!(
                        frames = st.frames,
                        skipped = st.skipped,
                        fps = st.fps,
                        "pipeline selesai"
                    ),
                    Err(e) => tracing::warn!("pipeline berhenti: {e}"),
                }
            });
        })
        .map_err(|e| format!("spawn thread pipeline: {e}"))?;

    Ok(PipelineHandle {
        stop: stop_tx,
        join,
    })
}

/// Resolusi parameter kirim (async) lalu jalankan pipeline di thread dedikasi.
async fn maybe_start_pipeline(
    pipeline: &mut Option<PipelineHandle>,
    peer: Option<&SenderPeer>,
    target: Option<&CaptureTarget>,
    video: VideoSettings,
    events: &mpsc::UnboundedSender<SessionEvent>,
    h264: H264Tuning,
) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err("peer tidak ada".to_string());
    };
    let target = target.ok_or_else(|| "sumber gambar belum dipilih".to_string())?;
    let (ssrc, payload_type) = peer
        .video_send_params()
        .await
        .ok_or_else(|| "parameter kirim video belum ternegosiasi".to_string())?;
    let handle = start_pipeline(peer, target, video, ssrc, payload_type, events, h264)?;
    *pipeline = Some(handle);
    Ok(())
}

/// Hentikan pipeline (idempoten) dan tunggu task berakhir.
async fn stop_pipeline(pipeline: &mut Option<PipelineHandle>) {
    if let Some(h) = pipeline.take() {
        let _ = h.stop.send(true);
        let join = h.join;
        // join() memblokir → jalankan di blocking pool agar tidak menghambat
        // runtime async.
        let res = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::task::spawn_blocking(move || {
                let _ = join.join();
            }),
        )
        .await;
        if res.is_err() {
            tracing::warn!("pipeline lambat berhenti (timeout)");
        }
    }
}

/// Teruskan semua event peer yang tersedia saat ini ke WS/UI.
/// Mengembalikan Err bila WS putus sehingga drive loop berhenti.
/// Mengembalikan `Ok(true)` bila `PeerConnected` terlihat pada pemanggilan ini.
async fn drain_peer(
    sink: &mut WsSink,
    peer: Option<&mut SenderPeer>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Result<bool, ()> {
    let Some(p) = peer else { return Ok(false) };
    let mut connected = false;
    loop {
        match p.next_event(Duration::from_millis(0)).await {
            Ok(PeerEvent::Ice(c)) => {
                if send_msg(sink, &ClientMsg::Ice { candidate: c })
                    .await
                    .is_err()
                {
                    let _ = events.send(SessionEvent::Ended {
                        reason: "ws-putus".into(),
                    });
                    return Err(());
                }
            }
            Ok(PeerEvent::Connected) => {
                connected = true;
                let _ = events.send(SessionEvent::PeerConnected);
            }
            Ok(PeerEvent::CtrlOpen) => {
                let _ = events.send(SessionEvent::CtrlOpen);
            }
            Ok(PeerEvent::CtrlMessage(m)) => {
                let _ = events.send(SessionEvent::CtrlMessage(m));
            }
            Err(_) => break, // tidak ada event saat ini
        }
    }
    Ok(connected)
}

/// Error server yang membuat sesi sender tidak lagi berguna (harus tutup).
/// Selain ini (unexpected/internalError) dianggap recoverable: sesi tetap
/// hidup dan bisa Start ulang.
fn is_fatal_error(code: &super::protocol::ErrorCode) -> bool {
    use super::protocol::ErrorCode;
    matches!(
        code,
        ErrorCode::BadToken | ErrorCode::SenderTaken | ErrorCode::ProtoMismatch
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_settings_default_matches_production_path() {
        let settings = VideoSettings::default();
        assert_eq!(settings.resolution, ResolutionPreset::P1080);
        assert_eq!(settings.fps, 30);
        assert_eq!(settings.quality, QualityPreset::Balanced);
        assert_eq!(settings.resolution.dimensions(), (1920, 1080));
        assert_eq!(settings.quality.quality(), 0.9);
        assert_eq!(settings.quality.bitrate_cap_bps(), 10_000_000);
        assert_eq!(QualityPreset::Sharp.quality(), 0.95);
        assert_eq!(QualityPreset::Sharp.bitrate_cap_bps(), 18_000_000);
    }
}
