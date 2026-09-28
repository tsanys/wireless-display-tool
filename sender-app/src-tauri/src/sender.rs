//! Backend sender-app: lifecycle signaling server + sesi sender + state UI.
//!
//! Alur start (di `init_sender`):
//! signaling server (0.0.0.0:8420) → advertise mDNS → WS loopback sebagai
//! sender → forwarder event ke frontend via Tauri `emit`. Semua kontrak
//! pesan mengikuti `docs/SIGNALING_PROTOCOL.md` (via wdt-core).

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, Mutex};
use wdt_core::signaling::discovery::MdnsAdvert;
use wdt_core::signaling::protocol::{AudioRoute, ReceiverCaps, DEFAULT_PORT};
use wdt_core::signaling::sender_session::{
    self, CaptureTarget, SenderMode, SessionCmd, SessionEvent,
};
use wdt_core::signaling::server::{self, SignalingServer};

/// Status mirroring untuk UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MirrorState {
    Idle,
    Offering,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorStatus {
    pub state: MirrorState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingInfo {
    pub ip: String,
    pub port: u16,
    pub token: String,
    pub pairing_string: String,
    pub mdns_instance: String,
    pub server_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_error: Option<String>,
}

/// Satu display yang dapat dipilih sebagai sumber gambar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub is_primary: bool,
    pub width: u32,
    pub height: u32,
}

/// Status capability yang belum tentu tersedia di semua host.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityState {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Snapshot kemampuan backend untuk mengontrol opsi yang ditampilkan UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub displays: Vec<DisplayInfo>,
    pub virtual_display: CapabilityState,
    pub system_audio_capture: CapabilityState,
    pub receiver_audio: bool,
}

/// Mode display yang diminta user untuk satu sesi berbagi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DisplayMode {
    Mirror {
        display_id: String,
    },
    Extended {
        width: u32,
        height: u32,
        refresh_hz: u32,
    },
}

/// Preset kualitas encoder user-facing (R7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QualityPreset {
    /// Seimbang (default; kualitas 0.9, cap 10 Mbps).
    Balanced,
    /// Tajam untuk teks (kualitas 0.95, cap 18 Mbps — butuh LAN stabil).
    Sharp,
}

/// Resolusi stream user-facing (R7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolutionPreset {
    /// 1080p (default).
    P1080,
    /// 720p (hemat CPU/jaringan).
    P720,
}

/// Frame rate stream user-facing (R7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FrameRatePreset {
    Fps30,
    Fps60,
}

/// Pengaturan video user-facing (dipetakan ke `wdt_core` VideoSettings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSettingsUi {
    pub resolution: ResolutionPreset,
    pub frame_rate: FrameRatePreset,
    pub quality: QualityPreset,
}

impl Default for VideoSettingsUi {
    fn default() -> Self {
        Self {
            resolution: ResolutionPreset::P1080,
            frame_rate: FrameRatePreset::Fps30,
            quality: QualityPreset::Balanced,
        }
    }
}

impl VideoSettingsUi {
    /// Pemetaan ke tipe core.
    pub fn to_core(self) -> wdt_core::signaling::sender_session::VideoSettings {
        use wdt_core::signaling::sender_session::{
            QualityPreset as CoreQuality, ResolutionPreset as CoreResolution, VideoSettings,
        };
        VideoSettings {
            resolution: match self.resolution {
                ResolutionPreset::P1080 => CoreResolution::P1080,
                ResolutionPreset::P720 => CoreResolution::P720,
            },
            fps: match self.frame_rate {
                FrameRatePreset::Fps30 => 30,
                FrameRatePreset::Fps60 => 60,
            },
            quality: match self.quality {
                QualityPreset::Balanced => CoreQuality::Balanced,
                QualityPreset::Sharp => CoreQuality::Sharp,
            },
        }
    }

    /// Validasi terhadap batas encoder (gating capability).
    pub fn validate(&self) -> Result<(), String> {
        let (w, h) = match self.resolution {
            ResolutionPreset::P1080 => (1920u32, 1080u32),
            ResolutionPreset::P720 => (1280u32, 720u32),
        };
        if w > wdt_core::encode::ENCODER_MAX_WIDTH || h > wdt_core::encode::ENCODER_MAX_HEIGHT {
            return Err(format!(
                "Encoder mendukung maksimum {}×{}; resolusi {w}×{h} terlalu besar",
                wdt_core::encode::ENCODER_MAX_WIDTH,
                wdt_core::encode::ENCODER_MAX_HEIGHT
            ));
        }
        let fps = match self.frame_rate {
            FrameRatePreset::Fps30 => 30,
            FrameRatePreset::Fps60 => 60,
        };
        if fps > wdt_core::encode::ENCODER_MAX_FPS {
            return Err(format!(
                "Encoder mendukung maksimum {} fps",
                wdt_core::encode::ENCODER_MAX_FPS
            ));
        }
        Ok(())
    }
}

/// Konfigurasi immutable yang dipakai sepanjang satu sesi share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSettings {
    pub receiver_id: String,
    pub display_mode: DisplayMode,
    pub audio_route: AudioRoute,
    #[serde(default)]
    pub video: VideoSettingsUi,
}

impl ShareSettings {
    fn mirror_defaults(receiver_id: String) -> Self {
        Self {
            receiver_id,
            display_mode: DisplayMode::Mirror {
                display_id: "main".to_string(),
            },
            audio_route: AudioRoute::Laptop,
            video: VideoSettingsUi::default(),
        }
    }
}

/// Snapshot pipeline untuk diagnostik (tanpa data sensitif).
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct PipelineSnapshot {
    width: u32,
    height: u32,
    fps: u32,
    ssrc: u32,
    payload_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    frames: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    measured_fps: Option<f32>,
}

/// Snapshot audio untuk diagnostik.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct AudioSnapshot {
    packets_sent: u64,
    sent_ms: u64,
    dropped: u64,
    errors: u64,
    reconnects: u64,
    level: f32,
}

/// Masukan murni untuk membangun berkas diagnostik (tanpa I/O) — dapat diuji.
struct DiagnosticsInput {
    receiver_id: Option<String>,
    receiver_audio: bool,
    display_mode: Option<String>,
    audio_route: Option<String>,
    mirror_state: String,
    server_running: bool,
    pipeline: Option<PipelineSnapshot>,
    audio: Option<AudioSnapshot>,
}

/// Bangun berkas diagnostik lokal.
///
/// Sengaja TIDAK memuat token pairing, alamat IP, atau kredensial apa pun —
/// berkas ini hanya berisi state teknis untuk pemeriksaan masalah.
fn diagnostics_bundle(input: DiagnosticsInput) -> serde_json::Value {
    serde_json::json!({
        "generatedBy": "wdt-sender",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "privacy": {
            "redactedPairingCode": true,
            "redactedNetworkAddresses": true,
            "note": "Berkas ini disimpan lokal dan tidak dikirim ke jaringan."
        },
        "server": {
            "running": input.server_running,
        },
        "session": {
            "mirrorState": input.mirror_state,
            "displayMode": input.display_mode,
            "audioRoute": input.audio_route,
            "receiverConnected": input.receiver_id.is_some(),
            "receiverAudioSupported": input.receiver_audio,
        },
        "pipeline": input.pipeline,
        "audio": input.audio,
    })
}

struct Inner {
    server_addr: Option<SocketAddr>,
    token: Option<String>,
    mdns_instance: Option<String>,
    server_error: Option<String>,
    session_cmds: Option<mpsc::UnboundedSender<SessionCmd>>,
    receiver: Option<ReceiverView>,
    /// Kemampuan receiver aktif (None = belum terhubung / receiver lama).
    receiver_caps: Option<ReceiverCaps>,
    /// Snapshot diagnostik terakhir (diisi dari event pipeline/audio).
    pipeline_snapshot: Option<PipelineSnapshot>,
    audio_snapshot: Option<AudioSnapshot>,
    mirror: MirrorStatus,
    active_settings: Option<ShareSettings>,
    // mDNS advert + server harus hidup selama app; disimpan agar tidak drop.
    _advert: Option<MdnsAdvert>,
    _server: Option<SignalingServer>,
}

/// State global app (di-manage Tauri).
pub struct AppState {
    inner: Mutex<Inner>,
}

impl AppState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                server_addr: None,
                token: None,
                mdns_instance: None,
                server_error: None,
                session_cmds: None,
                receiver: None,
                receiver_caps: None,
                pipeline_snapshot: None,
                audio_snapshot: None,
                mirror: MirrorStatus {
                    state: MirrorState::Idle,
                    message: None,
                },
                active_settings: None,
                _advert: None,
                _server: None,
            }),
        }
    }
}

/// Dipanggil sekali dari `setup`: jalankan server + advertise + sesi loopback.
pub async fn init_sender(app: AppHandle) -> Result<(), String> {
    let state: tauri::State<'_, AppState> = app.state();

    // 1. Signaling server di semua interface — token dipersist di app-data
    //    agar restart tidak mengubah token (TV tidak perlu ketik ulang).
    let token_path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("pairing-token"));
    let persisted = token_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| t.len() == 6 && t.chars().all(|c| c.is_ascii_digit()));

    let server =
        match server::spawn_with_token(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)), persisted)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("bind 0.0.0.0:{DEFAULT_PORT} gagal: {e}");
                state.inner.lock().await.server_error = Some(msg.clone());
                app.emit(
                    "server-status",
                    serde_json::json!({"running": false, "error": msg}),
                )
                .map_err(|e| e.to_string())?;
                return Err(msg);
            }
        };
    let port = server.local_addr.port();
    let token = server.token.clone();
    // Simpan token bila berubah agar restart berikutnya memakai token sama.
    if let Some(path) = &token_path {
        if std::fs::read_to_string(path).ok().as_deref() != Some(token.as_str()) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, &token);
        }
    }

    // 2. Advertise mDNS.
    let ip = local_ip();
    let hostname = hostname();
    let instance = format!("WDT {}", short_host(&hostname));
    let advert = MdnsAdvert::advertise(&instance, &format!("{hostname}.local."), &ip, port)
        .map_err(|e| format!("mDNS advertise gagal: {e}"))?;

    // 3. Sesi sender via loopback.
    let url = format!("ws://127.0.0.1:{port}/ws");
    let mut session = sender_session::spawn_sender_session(&url, SenderMode::VideoOnly)
        .await
        .map_err(|e| format!("loopback session gagal: {e}"))?;
    let cmds = session.cmds.clone();

    {
        let mut inner = state.inner.lock().await;
        inner.server_addr = Some(server.local_addr);
        inner.token = Some(token.clone());
        inner.mdns_instance = Some(instance.clone());
        inner.session_cmds = Some(cmds);
        inner._advert = Some(advert);
        inner._server = Some(server);
    }

    app.emit(
        "server-status",
        serde_json::json!({
            "running": true,
            "ip": ip,
            "port": port,
            "token": token,
            "pairingString": format!("{ip}:{port}:{token}"),
            "mdnsInstance": instance,
        }),
    )
    .map_err(|e| e.to_string())?;

    // 4. Forwarder event sesi → state + emit ke frontend.
    let app2 = app.clone();
    tokio::spawn(async move {
        while let Some(ev) = session.events.recv().await {
            handle_session_event(&app2, ev).await;
        }
    });

    Ok(())
}

async fn handle_session_event(app: &AppHandle, ev: SessionEvent) {
    let state: tauri::State<'_, AppState> = app.state();
    match ev {
        SessionEvent::HelloOk { .. } => {}
        SessionEvent::ReceiverJoined { device_id, caps } => {
            let mut inner = state.inner.lock().await;
            inner.receiver = Some(ReceiverView {
                device_id: device_id.clone(),
            });
            inner.receiver_caps = caps;
            let _ = app.emit(
                "receiver-joined",
                serde_json::json!({
                    "deviceId": device_id,
                    "audioSupported": caps.map(|c| c.audio).unwrap_or(false),
                }),
            );
        }
        SessionEvent::ReceiverLeft { reason } => {
            let mut inner = state.inner.lock().await;
            inner.receiver = None;
            inner.receiver_caps = None;
            inner.active_settings = None;
            if inner.mirror.state != MirrorState::Idle {
                inner.mirror = MirrorStatus {
                    state: MirrorState::Idle,
                    message: Some(reason.clone()),
                };
                let _ = app.emit(
                    "mirror-status",
                    serde_json::json!({ "state": "idle", "message": reason }),
                );
            }
            drop(inner);
            let _ = app.emit("receiver-left", serde_json::json!({ "reason": reason }));
        }
        SessionEvent::OfferSent => {
            set_mirror(app, MirrorState::Offering, None).await;
        }
        SessionEvent::AnswerReceived => {
            set_mirror(app, MirrorState::Connecting, None).await;
        }
        SessionEvent::PeerConnected => {
            set_mirror(app, MirrorState::Connected, None).await;
        }
        SessionEvent::CtrlOpen | SessionEvent::CtrlMessage(_) => {}
        SessionEvent::PipelineStarted {
            width,
            height,
            fps,
            ssrc,
            payload_type,
        } => {
            // Diagnosa interop: PT harus sama dengan yang diiklankan di SDP.
            // Dicetak ke stderr agar terlihat di konsol `tauri dev`.
            eprintln!("[wdt] pipeline mulai: {width}x{height}@{fps} ssrc={ssrc} pt={payload_type}");
            {
                let mut inner = state.inner.lock().await;
                inner.pipeline_snapshot = Some(PipelineSnapshot {
                    width,
                    height,
                    fps,
                    ssrc,
                    payload_type,
                    ..Default::default()
                });
            }
            let _ = app.emit(
                "mirror-pipeline",
                serde_json::json!({
                    "width": width, "height": height, "fps": fps,
                    "ssrc": ssrc, "payloadType": payload_type,
                }),
            );
        }
        SessionEvent::PipelineStats(st) => {
            {
                let mut inner = state.inner.lock().await;
                let snap = inner.pipeline_snapshot.get_or_insert_with(Default::default);
                snap.frames = Some(st.frames);
                snap.skipped = Some(st.skipped);
                snap.errors = Some(st.errors);
                snap.measured_fps = Some(st.fps);
            }
            // Statistik streaming (~tiap 2 dtk) untuk indikator UI.
            let _ = app.emit(
                "mirror-stats",
                serde_json::json!({
                    "frames": st.frames,
                    "skipped": st.skipped,
                    "errors": st.errors,
                    "fps": st.fps,
                }),
            );
        }
        SessionEvent::AudioStats(st) => {
            {
                let mut inner = state.inner.lock().await;
                inner.audio_snapshot = Some(AudioSnapshot {
                    packets_sent: st.packets_sent,
                    sent_ms: st.sent_ms,
                    dropped: st.dropped,
                    errors: st.errors,
                    reconnects: st.reconnects,
                    level: st.level,
                });
            }
            // Statistik audio (~tiap 2 dtk): indikator level + jumlah paket.
            let _ = app.emit(
                "audio-stats",
                serde_json::json!({
                    "packetsSent": st.packets_sent,
                    "sentMs": st.sent_ms,
                    "dropped": st.dropped,
                    "errors": st.errors,
                    "reconnects": st.reconnects,
                    "level": st.level,
                }),
            );
        }
        SessionEvent::AudioRouteChanged { route } => {
            // Route audio aktif (live switching) — UI mengonfirmasi.
            let _ = app.emit("audio-route-changed", serde_json::json!({ "route": route }));
        }
        SessionEvent::AudioDegraded { message } => {
            // Non-fatal: video tetap jalan. UI menampilkan peringatan.
            eprintln!("[wdt] audio degraded: {message}");
            let _ = app.emit("audio-degraded", serde_json::json!({ "message": message }));
        }
        SessionEvent::MirroringStopped => {
            let mut inner = state.inner.lock().await;
            inner.active_settings = None;
            inner.pipeline_snapshot = None;
            inner.audio_snapshot = None;
            inner.mirror = MirrorStatus {
                state: MirrorState::Idle,
                message: None,
            };
            let status = inner.mirror.clone();
            drop(inner);
            let _ = app.emit("mirror-status", status);
        }
        SessionEvent::Ended { reason } => {
            let mut inner = state.inner.lock().await;
            inner.active_settings = None;
            inner.pipeline_snapshot = None;
            inner.audio_snapshot = None;
            if inner.mirror.state != MirrorState::Idle {
                inner.mirror = MirrorStatus {
                    state: MirrorState::Idle,
                    message: Some(reason),
                };
                let status = inner.mirror.clone();
                drop(inner);
                let _ = app.emit("mirror-status", status);
            }
        }
        SessionEvent::Error(msg) => {
            set_mirror(app, MirrorState::Error, Some(msg)).await;
        }
    }
}

async fn set_mirror(app: &AppHandle, state_: MirrorState, message: Option<String>) {
    let state: tauri::State<'_, AppState> = app.state();
    let mut inner = state.inner.lock().await;
    inner.mirror = MirrorStatus {
        state: state_,
        message,
    };
    let status = inner.mirror.clone();
    drop(inner);
    let _ = app.emit("mirror-status", status);
}

// ---------- Tauri commands ----------

#[tauri::command]
pub async fn get_pairing_info(state: tauri::State<'_, AppState>) -> Result<PairingInfo, String> {
    let inner = state.inner.lock().await;
    match (&inner.server_addr, &inner.token, &inner.mdns_instance) {
        (Some(addr), Some(token), Some(instance)) => {
            let ip = local_ip();
            let port = addr.port();
            Ok(PairingInfo {
                ip: ip.clone(),
                port,
                token: token.clone(),
                pairing_string: format!("{ip}:{port}:{token}"),
                mdns_instance: instance.clone(),
                server_running: true,
                server_error: None,
            })
        }
        _ => Ok(PairingInfo {
            ip: String::new(),
            port: DEFAULT_PORT,
            token: String::new(),
            pairing_string: String::new(),
            mdns_instance: String::new(),
            server_running: false,
            server_error: inner.server_error.clone(),
        }),
    }
}

#[tauri::command]
pub async fn get_receivers(state: tauri::State<'_, AppState>) -> Result<Vec<ReceiverView>, String> {
    let inner = state.inner.lock().await;
    Ok(inner.receiver.clone().into_iter().collect())
}

#[tauri::command]
pub async fn get_mirror_status(state: tauri::State<'_, AppState>) -> Result<MirrorStatus, String> {
    Ok(state.inner.lock().await.mirror.clone())
}

/// Capability nyata saat ini.
///
/// `system_audio_capture` berasal dari probe platform (versi OS + izin);
/// `receiver_audio` berasal dari caps receiver yang sedang terhubung —
/// bukan konstanta `true`. Extended tetap dilaporkan belum tersedia.
#[tauri::command]
pub async fn get_capabilities(
    state: tauri::State<'_, AppState>,
) -> Result<CapabilityReport, String> {
    let displays = wdt_core::capture::available_displays()
        .map_err(|e| format!("Tidak dapat membaca daftar layar: {e}"))?
        .into_iter()
        .map(|display| DisplayInfo {
            id: display.id,
            name: display.name,
            is_primary: display.is_primary,
            width: display.width,
            height: display.height,
        })
        .collect();
    let audio_probe = wdt_core::audio::capability();
    let vdisplay_probe = wdt_core::vdisplay::capability();
    let receiver_audio = {
        let inner = state.inner.lock().await;
        inner.receiver_caps.map(|c| c.audio).unwrap_or(false)
    };
    Ok(CapabilityReport {
        displays,
        virtual_display: CapabilityState {
            available: vdisplay_probe.available,
            reason: vdisplay_probe.reason,
        },
        system_audio_capture: CapabilityState {
            available: audio_probe.available,
            reason: audio_probe.reason,
        },
        receiver_audio,
    })
}

/// Petakan mode display user → sumber capture pipeline.
fn capture_target_for(mode: &DisplayMode) -> CaptureTarget {
    match mode {
        DisplayMode::Mirror { display_id } => CaptureTarget::Display(display_id.clone()),
        DisplayMode::Extended {
            width,
            height,
            refresh_hz,
        } => CaptureTarget::Virtual {
            width: *width,
            height: *height,
            refresh_hz: *refresh_hz,
        },
    }
}

/// Validasi route audio terhadap capability nyata.
fn audio_route_allowed(route: AudioRoute, system_capture: bool, receiver_audio: bool) -> bool {
    if !route.sends_to_tv() {
        return true;
    }
    system_capture && receiver_audio
}

fn validate_share_settings(
    settings: &ShareSettings,
    receiver: &ReceiverView,
    receiver_caps: Option<ReceiverCaps>,
    displays: &[DisplayInfo],
    system_capture: bool,
    virtual_display: &CapabilityState,
) -> Result<(), String> {
    let connected_id = receiver.device_id.clone().unwrap_or_default();
    if !settings.receiver_id.is_empty() && settings.receiver_id != connected_id {
        return Err(format!(
            "TV '{}' tidak lagi terhubung",
            settings.receiver_id
        ));
    }
    match &settings.display_mode {
        DisplayMode::Mirror { display_id } => {
            let display_available = if display_id == "main" {
                displays.iter().any(|display| display.is_primary)
            } else {
                displays.iter().any(|display| display.id == *display_id)
            };
            if !display_available {
                return Err(
                    "Layar yang dipilih sudah tidak tersedia. Pilih layar lain lalu coba lagi."
                        .to_string(),
                );
            }
        }
        DisplayMode::Extended { .. } => {
            // Fitur eksperimental: hanya bila backend layar tambahan tersedia.
            if !virtual_display.available {
                return Err(virtual_display
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Layar tambahan belum tersedia di versi ini".to_string()));
            }
        }
    }
    // Validasi pengaturan video (gating encoder) — R7.
    settings.video.validate()?;
    let receiver_audio = receiver_caps.map(|c| c.audio).unwrap_or(false);
    if !audio_route_allowed(settings.audio_route, system_capture, receiver_audio) {
        if !system_capture {
            return Err(
                "Capture audio sistem belum siap di laptop ini. Periksa izin Screen Recording \
                 dan versi sistem, lalu coba lagi."
                    .to_string(),
            );
        }
        return Err(
            "TV ini belum mendukung audio. Perbarui aplikasi WDT Receiver di TV.".to_string(),
        );
    }
    Ok(())
}

/// Mulai share memakai snapshot settings tervalidasi. Backend tetap menjadi
/// sumber kebenaran capability; request buatan tangan tidak dapat mengaktifkan
/// Extended/audio sebelum implementasinya tersedia.
#[tauri::command]
pub async fn start_sharing(app: AppHandle, settings: ShareSettings) -> Result<(), String> {
    let audio_probe = wdt_core::audio::capability();
    let vd_probe = wdt_core::vdisplay::capability();
    let vd_state = CapabilityState {
        available: vd_probe.available,
        reason: vd_probe.reason.clone(),
    };
    // Sumber gambar: display fisik untuk Mirror, display virtual untuk Extended.
    let target = capture_target_for(&settings.display_mode);
    let state = app.state::<AppState>();
    let (cmds, audio_route, settings_video) = {
        let mut inner = state.inner.lock().await;
        let receiver = inner
            .receiver
            .as_ref()
            .ok_or_else(|| "Belum ada TV terhubung".to_string())?;
        let receiver_caps = inner.receiver_caps;
        let displays = wdt_core::capture::available_displays()
            .map_err(|e| format!("Tidak dapat membaca daftar layar: {e}"))?
            .into_iter()
            .map(|display| DisplayInfo {
                id: display.id,
                name: display.name,
                is_primary: display.is_primary,
                width: display.width,
                height: display.height,
            })
            .collect::<Vec<_>>();
        validate_share_settings(
            &settings,
            receiver,
            receiver_caps,
            &displays,
            audio_probe.available,
            &vd_state,
        )?;
        let cmds = inner
            .session_cmds
            .clone()
            .ok_or_else(|| "Sesi berbagi belum siap".to_string())?;
        let route = settings.audio_route;
        let settings_video = settings.video;
        inner.active_settings = Some(settings);
        (cmds, route, settings_video)
    };
    if cmds
        .send(SessionCmd::StartOffer {
            target,
            audio_route,
            video: settings_video.to_core(),
        })
        .is_err()
    {
        state.inner.lock().await.active_settings = None;
        return Err("Sesi berbagi berhenti. Jalankan ulang aplikasi.".to_string());
    }
    set_mirror(&app, MirrorState::Offering, None).await;
    Ok(())
}

/// Ubah tujuan suara saat sesi aktif (live switching, tanpa putus video).
#[tauri::command]
pub async fn set_audio_route(app: AppHandle, route: AudioRoute) -> Result<(), String> {
    let audio_probe = wdt_core::audio::capability();
    let state = app.state::<AppState>();
    let cmds = {
        let inner = state.inner.lock().await;
        let receiver_audio = inner.receiver_caps.map(|c| c.audio).unwrap_or(false);
        if !audio_route_allowed(route, audio_probe.available, receiver_audio) {
            if !audio_probe.available {
                return Err(
                    "Capture audio sistem belum siap di laptop ini. Periksa izin Screen \
                     Recording dan versi sistem."
                        .to_string(),
                );
            }
            return Err("TV ini belum mendukung audio.".to_string());
        }
        inner
            .session_cmds
            .clone()
            .ok_or_else(|| "Sesi berbagi belum siap".to_string())?
    };
    cmds.send(SessionCmd::SetAudioRoute(route))
        .map_err(|_| "Sesi berbagi berhenti. Jalankan ulang aplikasi.".to_string())?;
    Ok(())
}

/// Mulai mirroring ke receiver terhubung. `receiver_id` dicocokkan dengan
/// deviceId receiver aktif (kosong = pakai yang terhubung, untuk MVP 1 slot).
#[tauri::command]
pub async fn start_mirroring(app: AppHandle, receiver_id: String) -> Result<(), String> {
    start_sharing(app, ShareSettings::mirror_defaults(receiver_id)).await
}

#[tauri::command]
pub async fn stop_mirroring(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let cmds = {
        let inner = state.inner.lock().await;
        inner
            .session_cmds
            .clone()
            .ok_or_else(|| "sesi sender belum siap".to_string())?
    };
    cmds.send(SessionCmd::StopMirroring)
        .map_err(|_| "sesi sender mati".to_string())?;
    state.inner.lock().await.active_settings = None;
    set_mirror(&app, MirrorState::Idle, None).await;
    Ok(())
}

/// Ekspor berkas diagnostik **lokal** (tanpa cloud, tanpa token/IP).
///
/// Menulis JSON ke app-data `diagnostics/` dan mengembalikan path untuk UI.
#[tauri::command]
pub async fn export_diagnostics(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("tidak dapat menemukan folder aplikasi: {e}"))?
        .join("diagnostics");
    std::fs::create_dir_all(&dir).map_err(|e| format!("gagal membuat folder: {e}"))?;

    let bundle = {
        let inner = state.inner.lock().await;
        let (display_mode, audio_route) = match &inner.active_settings {
            Some(s) => (
                Some(
                    match &s.display_mode {
                        DisplayMode::Mirror { .. } => "mirror",
                        DisplayMode::Extended { .. } => "extended",
                    }
                    .to_string(),
                ),
                serde_json::to_value(s.audio_route)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string)),
            ),
            None => (None, None),
        };
        diagnostics_bundle(DiagnosticsInput {
            receiver_id: inner.receiver.as_ref().and_then(|r| r.device_id.clone()),
            receiver_audio: inner.receiver_caps.map(|c| c.audio).unwrap_or(false),
            display_mode,
            audio_route,
            mirror_state: format!("{:?}", inner.mirror.state).to_lowercase(),
            server_running: inner.server_addr.is_some(),
            pipeline: inner.pipeline_snapshot.clone(),
            audio: inner.audio_snapshot.clone(),
        })
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("wdt-diagnostics-{ts}.json"));
    let text =
        serde_json::to_string_pretty(&bundle).map_err(|e| format!("gagal menyusun berkas: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("gagal menulis berkas: {e}"))?;
    Ok(path.display().to_string())
}

/// Teks bantuan firewall sesuai OS (dipakai panel instruksi manual).
#[tauri::command]
pub fn get_firewall_help() -> String {
    #[cfg(target_os = "windows")]
    {
        "Windows Defender Firewall biasanya menampilkan dialog izin saat aplikasi \
         pertama kali membuka port. Jika dialog tidak muncul atau koneksi gagal:\n\
         1. Buka Settings > Privacy & Security > Windows Security > Firewall & network protection.\n\
         2. Klik 'Allow an app through firewall' > Change settings > Allow another app.\n\
         3. Tambahkan 'Wireless Display Sender' dan centang Private (dan Public bila perlu).\n\
         4. Pastikan laptop dan TV berada di jaringan Private yang sama (bukan Public dengan client isolation)."
            .to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "macOS biasanya menampilkan dialog 'Do you want to allow incoming network connections?' \
         saat pertama dijalankan — klik Allow. Jika koneksi gagal:\n\
         1. Buka System Settings > Network > Firewall, pastikan aplikasi diizinkan.\n\
         2. Pastikan laptop dan TV berada di jaringan lokal yang sama."
            .to_string()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "Pastikan firewall mengizinkan koneksi TCP masuk ke port 8420.".to_string()
    }
}

fn local_ip() -> String {
    // IP sumber koneksi UDP keluar (connect UDP tidak mengirim traffic).
    let sock = std::net::UdpSocket::bind("0.0.0.0:0");
    let Ok(sock) = sock else {
        return "127.0.0.1".to_string();
    };
    let _ = sock.connect("8.8.8.8:80");
    sock.local_addr()
        .map(|a| match a.ip() {
            IpAddr::V4(v) => v.to_string(),
            IpAddr::V6(v) => v.to_string(),
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "sender".to_string())
}

fn short_host(h: &str) -> String {
    h.split('.').next().unwrap_or(h).chars().take(11).collect()
}

pub fn build_state() -> AppState {
    AppState::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver() -> ReceiverView {
        ReceiverView {
            device_id: Some("tv-keluarga".to_string()),
        }
    }

    fn vd_unavailable() -> CapabilityState {
        CapabilityState {
            available: false,
            reason: Some("Layar tambahan hanya tersedia di macOS (eksperimental)".to_string()),
        }
    }

    fn vd_available() -> CapabilityState {
        CapabilityState {
            available: true,
            reason: None,
        }
    }

    fn displays() -> Vec<DisplayInfo> {
        vec![DisplayInfo {
            id: "test:primary".to_string(),
            name: "Layar utama".to_string(),
            is_primary: true,
            width: 1920,
            height: 1080,
        }]
    }

    #[test]
    fn current_share_contract_accepts_mirror_with_laptop_audio() {
        let settings = ShareSettings::mirror_defaults("tv-keluarga".to_string());
        // Laptop/Muted selalu boleh, tanpa syarat capability.
        assert_eq!(
            validate_share_settings(
                &settings,
                &receiver(),
                None,
                &displays(),
                false,
                &vd_unavailable()
            ),
            Ok(())
        );
    }

    #[test]
    fn tv_routes_require_capture_and_receiver_caps() {
        let mut settings = ShareSettings::mirror_defaults("tv-keluarga".to_string());
        settings.audio_route = AudioRoute::Tv;
        // Tanpa capture sistem.
        assert!(validate_share_settings(
            &settings,
            &receiver(),
            Some(ReceiverCaps { audio: true }),
            &displays(),
            false,
            &vd_unavailable(),
        )
        .is_err());
        // Capture ada tapi receiver tidak mendukung audio.
        assert!(validate_share_settings(
            &settings,
            &receiver(),
            None,
            &displays(),
            true,
            &vd_unavailable()
        )
        .is_err());
        assert!(validate_share_settings(
            &settings,
            &receiver(),
            Some(ReceiverCaps { audio: false }),
            &displays(),
            true,
            &vd_unavailable(),
        )
        .is_err());
        // Keduanya ada → boleh.
        assert_eq!(
            validate_share_settings(
                &settings,
                &receiver(),
                Some(ReceiverCaps { audio: true }),
                &displays(),
                true,
                &vd_unavailable(),
            ),
            Ok(())
        );
        // Both & Muted juga mengikuti aturan yang sama/aman.
        settings.audio_route = AudioRoute::Both;
        assert!(validate_share_settings(
            &settings,
            &receiver(),
            None,
            &displays(),
            true,
            &vd_unavailable()
        )
        .is_err());
        settings.audio_route = AudioRoute::Muted;
        assert_eq!(
            validate_share_settings(
                &settings,
                &receiver(),
                None,
                &displays(),
                false,
                &vd_unavailable()
            ),
            Ok(())
        );
    }

    #[test]
    fn capability_gate_rejects_unimplemented_modes() {
        let mut settings = ShareSettings::mirror_defaults("tv-keluarga".to_string());
        settings.display_mode = DisplayMode::Extended {
            width: 1920,
            height: 1080,
            refresh_hz: 30,
        };
        assert!(validate_share_settings(
            &settings,
            &receiver(),
            None,
            &displays(),
            false,
            &vd_unavailable()
        )
        .is_err());
    }

    /// Extended (layar tambahan) hanya boleh bila backend virtual display
    /// tersedia; pesan penolakan harus jelas.
    #[test]
    fn extended_requires_virtual_display_capability() {
        let mut settings = ShareSettings::mirror_defaults("tv-keluarga".to_string());
        settings.display_mode = DisplayMode::Extended {
            width: 1920,
            height: 1080,
            refresh_hz: 60,
        };

        let err = validate_share_settings(
            &settings,
            &receiver(),
            None,
            &displays(),
            false,
            &vd_unavailable(),
        )
        .expect_err("tanpa capability harus ditolak");
        assert!(
            err.to_lowercase().contains("layar tambahan"),
            "pesan harus menyebut layar tambahan: {err}"
        );

        assert_eq!(
            validate_share_settings(
                &settings,
                &receiver(),
                None,
                &displays(),
                false,
                &vd_available()
            ),
            Ok(())
        );
    }

    /// Mode display dipetakan ke sumber capture yang benar.
    #[test]
    fn display_mode_maps_to_capture_target() {
        assert_eq!(
            capture_target_for(&DisplayMode::Mirror {
                display_id: "cg:1".to_string()
            }),
            CaptureTarget::Display("cg:1".to_string())
        );
        assert_eq!(
            capture_target_for(&DisplayMode::Extended {
                width: 1920,
                height: 1080,
                refresh_hz: 60
            }),
            CaptureTarget::Virtual {
                width: 1920,
                height: 1080,
                refresh_hz: 60
            }
        );
    }

    #[test]
    fn selected_display_must_still_be_connected() {
        let mut settings = ShareSettings::mirror_defaults("tv-keluarga".to_string());
        settings.display_mode = DisplayMode::Mirror {
            display_id: "test:missing".to_string(),
        };
        assert!(validate_share_settings(
            &settings,
            &receiver(),
            None,
            &displays(),
            false,
            &vd_unavailable()
        )
        .is_err());

        settings.display_mode = DisplayMode::Mirror {
            display_id: "test:primary".to_string(),
        };
        assert_eq!(
            validate_share_settings(
                &settings,
                &receiver(),
                None,
                &displays(),
                false,
                &vd_unavailable()
            ),
            Ok(())
        );
    }

    #[test]
    fn audio_route_allowed_matrix() {
        assert!(audio_route_allowed(AudioRoute::Laptop, false, false));
        assert!(audio_route_allowed(AudioRoute::Muted, false, false));
        assert!(!audio_route_allowed(AudioRoute::Tv, false, false));
        assert!(!audio_route_allowed(AudioRoute::Tv, true, false));
        assert!(!audio_route_allowed(AudioRoute::Tv, false, true));
        assert!(audio_route_allowed(AudioRoute::Tv, true, true));
        assert!(audio_route_allowed(AudioRoute::Both, true, true));
    }

    /// Bundle diagnostik memuat state teknis dan **tidak** memuat token/IP.
    #[test]
    fn diagnostics_bundle_excludes_secrets_and_has_state() {
        let bundle = diagnostics_bundle(DiagnosticsInput {
            receiver_id: Some("tv-keluarga".to_string()),
            receiver_audio: true,
            display_mode: Some("mirror".to_string()),
            audio_route: Some("tv".to_string()),
            mirror_state: "connected".to_string(),
            server_running: true,
            pipeline: Some(PipelineSnapshot {
                width: 1920,
                height: 1080,
                fps: 30,
                ssrc: 111,
                payload_type: 102,
                frames: Some(10),
                ..Default::default()
            }),
            audio: Some(AudioSnapshot {
                packets_sent: 5,
                sent_ms: 100,
                ..Default::default()
            }),
        });

        assert_eq!(bundle["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(bundle["platform"], std::env::consts::OS);
        assert_eq!(bundle["session"]["mirrorState"], "connected");
        assert_eq!(bundle["session"]["audioRoute"], "tv");
        assert_eq!(bundle["pipeline"]["width"], 1920);
        assert_eq!(bundle["audio"]["packetsSent"], 5);

        // Tidak ada data sensitif/PII: periksa nama kunci (rekursif), pola
        // alamat IPv4, dan string kode pairing 6 digit.
        fn collect_keys(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, val) in map {
                        out.push(k.to_lowercase());
                        collect_keys(val, out);
                    }
                }
                serde_json::Value::Array(items) => {
                    for it in items {
                        collect_keys(it, out);
                    }
                }
                _ => {}
            }
        }
        let mut keys = Vec::new();
        collect_keys(&bundle, &mut keys);
        for key in &keys {
            for forbidden in [
                "token",
                "ip",
                "ipaddress",
                "password",
                "secret",
                "credential",
            ] {
                assert!(
                    key != forbidden,
                    "kunci '{key}' tidak boleh ada di bundle diagnostik"
                );
            }
        }

        let text = serde_json::to_string(&bundle).unwrap();
        // Tidak ada alamat IPv4 (mis. "10.10.70.24").
        let looks_like_ipv4 = text
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .any(|part| {
                let octets: Vec<&str> = part.split('.').collect();
                octets.len() == 4
                    && octets.iter().all(|o| {
                        !o.is_empty() && o.len() <= 3 && o.parse::<u16>().is_ok_and(|n| n <= 255)
                    })
            });
        assert!(
            !looks_like_ipv4,
            "bundle tidak boleh memuat alamat IP: {text}"
        );
        // Tidak ada kode pairing 6 digit.
        assert!(
            !text.contains("123456"),
            "bundle tidak boleh memuat kode pairing: {text}"
        );
    }

    #[test]
    fn share_settings_json_matches_typescript_contract() {
        let json = serde_json::json!({
            "receiverId": "tv-keluarga",
            "displayMode": { "kind": "mirror", "displayId": "main" },
            "audioRoute": "laptop",
            "video": { "resolution": "p1080", "frameRate": "fps30", "quality": "balanced" }
        });
        let parsed: ShareSettings = serde_json::from_value(json).expect("contract JSON");
        assert_eq!(
            parsed,
            ShareSettings::mirror_defaults("tv-keluarga".to_string())
        );
    }

    /// Pemetaan UI → core VideoSettings konsisten.
    #[test]
    fn video_settings_ui_maps_to_core() {
        let ui = VideoSettingsUi {
            resolution: ResolutionPreset::P720,
            frame_rate: FrameRatePreset::Fps60,
            quality: QualityPreset::Sharp,
        };
        let core = ui.to_core();
        assert_eq!(
            core,
            wdt_core::signaling::sender_session::VideoSettings {
                resolution: wdt_core::signaling::sender_session::ResolutionPreset::P720,
                fps: 60,
                quality: wdt_core::signaling::sender_session::QualityPreset::Sharp,
            }
        );
        assert!(ui.validate().is_ok());
        assert!(VideoSettingsUi::default().validate().is_ok());
    }

    /// Bundel dapat ditulis ke berkas dan dibaca kembali (validasi I/O dasar).
    #[test]
    fn diagnostics_bundle_roundtrips_through_file() {
        let bundle = diagnostics_bundle(DiagnosticsInput {
            receiver_id: None,
            receiver_audio: false,
            display_mode: None,
            audio_route: None,
            mirror_state: "idle".to_string(),
            server_running: true,
            pipeline: None,
            audio: None,
        });
        let text = serde_json::to_string_pretty(&bundle).unwrap();
        let path = std::env::temp_dir().join(format!("wdt-diag-test-{}.json", std::process::id()));
        std::fs::write(&path, &text).expect("tulis berkas");
        let read_back = std::fs::read_to_string(&path).expect("baca berkas");
        let _ = std::fs::remove_file(&path);
        let parsed: serde_json::Value = serde_json::from_str(&read_back).expect("parse");
        assert_eq!(parsed["session"]["mirrorState"], "idle");
        assert_eq!(parsed["privacy"]["redactedPairingCode"], true);
    }
}
