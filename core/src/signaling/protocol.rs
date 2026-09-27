//! Kontrak pesan signaling WDT — single source of truth.
//!
//! Representasi wire: JSON object dengan discriminator `type`
//! (serde tag internal), field camelCase. Dokumen human-readable:
//! `docs/SIGNALING_PROTOCOL.md` — keduanya harus dijaga sinkron;
//! perubahan protokol wajib mengubah keduanya + bump `PROTO_VERSION`.

use serde::{Deserialize, Serialize};

/// Versi protokol signaling. Receiver dengan versi berbeda ditolak
/// dengan `error protoMismatch`.
pub const PROTO_VERSION: u32 = 1;

/// Port TCP default signaling server (bind 0.0.0.0).
pub const DEFAULT_PORT: u16 = 8420;

/// Path endpoint WebSocket.
pub const WS_PATH: &str = "/ws";

/// Service type mDNS yang di-advertise sender (DNS-SD).
pub const MDNS_SERVICE_TYPE: &str = "_wdt._tcp.local.";

/// Peran koneksi WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// Sender app (connect via loopback saja).
    Sender,
    /// Receiver app di TV.
    Receiver,
}

/// Tempat suara sesi diputar (kanonik, dipakai core + UI sender).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioRoute {
    /// Audio lokal hidup; tidak ada media audio ke TV.
    Laptop,
    /// Audio sistem dikirim ke TV.
    Tv,
    /// Audio lokal hidup dan audio dikirim ke TV.
    Both,
    /// Tidak ada media audio ke TV; output OS tidak diubah.
    Muted,
}

impl AudioRoute {
    /// Apakah route ini mengirim sample audio ke TV.
    pub fn sends_to_tv(self) -> bool {
        matches!(self, AudioRoute::Tv | AudioRoute::Both)
    }

    /// Label user-facing (Indonesia).
    pub fn label(self) -> &'static str {
        match self {
            AudioRoute::Laptop => "Laptop",
            AudioRoute::Tv => "TV",
            AudioRoute::Both => "Keduanya",
            AudioRoute::Muted => "Tanpa suara",
        }
    }
}

/// Kemampuan receiver yang relevan untuk negosiasi (opsional di `hello`).
///
/// Field ini **opsional**: receiver lama tidak mengirimnya → sender tetap
/// memakai offer video-only (tanpa regresi). Serde mengabaikan field asing,
/// sehingga sender lama yang menerima `caps` juga aman.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverCaps {
    /// Receiver dapat memutar audio Opus (libwebrtc AudioTrack + audio focus).
    pub audio: bool,
}

/// Konfigurasi audio sesi yang dikirim sender → receiver lewat signaling.
///
/// Dikirim saat offer dan tiap route berubah (live switching tanpa
/// renegosiasi). Receiver lama mem-parse ini sebagai tipe tak dikenal dan
/// mengabaikannya.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSessionConfig {
    /// Apakah audio media benar-benar aktif (route TV/Both).
    pub enabled: bool,
    /// Route yang dipilih user.
    pub route: AudioRoute,
    pub channels: u16,
    pub sample_rate: u32,
    /// Codec media (selalu "opus" di protokol v1).
    pub codec: String,
}

impl AudioSessionConfig {
    /// Konfigurasi kanonik untuk route tertentu (Opus 48 kHz stereo).
    pub fn for_route(route: AudioRoute) -> Self {
        Self {
            enabled: route.sends_to_tv(),
            route,
            channels: crate::audio::OUT_CHANNELS,
            sample_rate: crate::audio::OUT_SAMPLE_RATE,
            codec: "opus".to_string(),
        }
    }
}

/// Kandidat ICE (trickle) — pasangan field standar WebRTC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceCandidate {
    /// SDP candidate string, mis. "candidate:1 udp 2130706431 ...".
    pub candidate: String,
    /// Media ID, biasanya "0".
    #[serde(rename = "sdpMid")]
    pub sdp_mid: String,
    /// Index m-line, biasanya 0 (nama standar WebRTC: sdpMLineIndex).
    #[serde(rename = "sdpMLineIndex")]
    pub sdp_mline_index: u32,
}

/// Pesan client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMsg {
    /// Handshake pertama. Receiver wajib membawa `token` pairing yang
    /// valid; sender hanya boleh connect dari loopback dan tanpa token.
    Hello {
        role: Role,
        proto: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", rename = "deviceId")]
        device_id: Option<String>,
        /// Kemampuan receiver (opsional; absen = receiver lama, video-only).
        #[serde(skip_serializing_if = "Option::is_none")]
        caps: Option<ReceiverCaps>,
    },
    /// SDP offer — hanya dari sender (di-relay ke receiver aktif).
    Offer { sdp: String },
    /// SDP answer — hanya dari receiver (di-relay ke sender).
    Answer { sdp: String },
    /// ICE candidate trickle — dua arah, di-relay ke lawan.
    Ice { candidate: IceCandidate },
    /// Konfigurasi sesi (audio aktif/route) — sender → receiver.
    /// Dikirim saat offer dan setiap route berubah.
    SessionConfig { audio: AudioSessionConfig },
    /// Tutup sesi secara sopan.
    Bye {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Kode error yang stabil antar versi — jangan renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// Token pairing salah/absen.
    BadToken,
    /// Slot sender sudah terisi koneksi lain.
    SenderTaken,
    /// Sudah ada receiver aktif; sender hanya melayani satu receiver.
    SenderBusy,
    /// Versi `proto` tidak cocok.
    ProtoMismatch,
    /// Pesan tidak valid pada state koneksi ini.
    Unexpected,
    /// Kesalahan internal server.
    InternalError,
}

/// Pesan server → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMsg {
    /// Balasan hello yang diterima.
    HelloOk { proto: u32, server: String },
    /// Receiver baru terhubung (dikirim ke sender). Dipakai UI sender
    /// untuk menampilkan TV yang siap dihubungkan.
    ReceiverJoined {
        #[serde(skip_serializing_if = "Option::is_none", rename = "deviceId")]
        device_id: Option<String>,
        /// Kemampuan receiver (diteruskan dari `hello`; absen = lama).
        #[serde(skip_serializing_if = "Option::is_none")]
        caps: Option<ReceiverCaps>,
    },
    /// Relay SDP offer (dari sender) ke receiver.
    Offer { sdp: String },
    /// Relay SDP answer (dari receiver) ke sender.
    Answer { sdp: String },
    /// Relay ICE candidate ke lawan.
    Ice { candidate: IceCandidate },
    /// Relay konfigurasi sesi audio (sender → receiver).
    SessionConfig { audio: AudioSessionConfig },
    /// Error; server boleh menutup koneksi setelah ini.
    Error { code: ErrorCode, message: String },
    /// Notifikasi lawan disconnect / sesi berakhir.
    Bye { reason: String },
}

/// String identitas server untuk field `helloOk.server`.
pub fn server_ident() -> String {
    format!("wdt/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contoh wire-format — kalau test ini gagal setelah refactor serde,
    /// dokumen SIGNALING_PROTOCOL.md ikut diverifikasi.
    #[test]
    fn wire_format_stable() {
        let hello = ClientMsg::Hello {
            role: Role::Receiver,
            proto: 1,
            token: Some("123456".to_string()),
            device_id: Some("tv-living".to_string()),
            caps: None,
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","role":"receiver","proto":1,"token":"123456","deviceId":"tv-living"}"#
        );

        let ice = ServerMsg::Ice {
            candidate: IceCandidate {
                candidate: "candidate:1 udp 1 192.168.1.5 50000 typ host".into(),
                sdp_mid: "0".into(),
                sdp_mline_index: 0,
            },
        };
        assert_eq!(
            serde_json::to_string(&ice).unwrap(),
            r#"{"type":"ice","candidate":{"candidate":"candidate:1 udp 1 192.168.1.5 50000 typ host","sdpMid":"0","sdpMLineIndex":0}}"#
        );

        let err: ServerMsg = serde_json::from_str(
            r#"{"type":"error","code":"senderBusy","message":"receiver already active"}"#,
        )
        .unwrap();
        match err {
            ServerMsg::Error { code, .. } => assert_eq!(code, ErrorCode::SenderBusy),
            other => panic!("parse salah: {other:?}"),
        }
    }

    /// `caps` receiver terkirim saat ada dan absen saat None.
    #[test]
    fn receiver_caps_wire_format() {
        let hello = ClientMsg::Hello {
            role: Role::Receiver,
            proto: 1,
            token: Some("123456".to_string()),
            device_id: None,
            caps: Some(ReceiverCaps { audio: true }),
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","role":"receiver","proto":1,"token":"123456","caps":{"audio":true}}"#
        );

        let joined = ServerMsg::ReceiverJoined {
            device_id: Some("tv".into()),
            caps: Some(ReceiverCaps { audio: true }),
        };
        assert_eq!(
            serde_json::to_string(&joined).unwrap(),
            r#"{"type":"receiverJoined","deviceId":"tv","caps":{"audio":true}}"#
        );
    }

    /// Backward-compat: hello receiver LAMA (tanpa caps) tetap parse.
    #[test]
    fn legacy_hello_without_caps_parses() {
        let msg: ClientMsg = serde_json::from_str(
            r#"{"type":"hello","role":"receiver","proto":1,"token":"123456","deviceId":"tv-old"}"#,
        )
        .unwrap();
        match msg {
            ClientMsg::Hello {
                caps, device_id, ..
            } => {
                assert_eq!(caps, None);
                assert_eq!(device_id.as_deref(), Some("tv-old"));
            }
            other => panic!("parse salah: {other:?}"),
        }
    }

    /// `sessionConfig` wire format + semantic route.
    #[test]
    fn session_config_wire_format() {
        let cfg = AudioSessionConfig::for_route(AudioRoute::Tv);
        assert!(cfg.enabled);
        assert_eq!(cfg.channels, 2);
        assert_eq!(cfg.sample_rate, 48_000);
        assert_eq!(cfg.codec, "opus");

        let msg = ClientMsg::SessionConfig { audio: cfg };
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"sessionConfig","audio":{"enabled":true,"route":"tv","channels":2,"sampleRate":48000,"codec":"opus"}}"#
        );

        // Route laptop/muted → enabled false.
        assert!(!AudioSessionConfig::for_route(AudioRoute::Laptop).enabled);
        assert!(!AudioSessionConfig::for_route(AudioRoute::Muted).enabled);
        assert!(AudioSessionConfig::for_route(AudioRoute::Both).enabled);
    }

    /// Semantik route: hanya TV/Both mengirim ke TV.
    #[test]
    fn route_semantics() {
        assert!(!AudioRoute::Laptop.sends_to_tv());
        assert!(AudioRoute::Tv.sends_to_tv());
        assert!(AudioRoute::Both.sends_to_tv());
        assert!(!AudioRoute::Muted.sends_to_tv());
        assert_eq!(AudioRoute::Muted.label(), "Tanpa suara");
    }
}
