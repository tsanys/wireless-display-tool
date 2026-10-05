//! PeerConnection webrtc-rs untuk sisi sender & receiver-test.
//!
//! Menyediakan builder PC (STUN publik, opsional DataChannel `ctrl`,
//! video track H.264) plus helper offer/answer/ICE trickle. Dipakai oleh
//! jalur produksi (T4) dan test client T3.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine,
};
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::rtp_transceiver::{PayloadType, SSRC};
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
    RTCSessionDescription,
};

use super::protocol::IceCandidate;
use super::sdp_util;
use crate::encode::H264Profile;

const STUN_SERVER: &str = "stun:stun.l.google.com:19302";
const CTRL_LABEL: &str = "ctrl";
const H264_CLOCK_RATE: u32 = 90_000;
const H264_PAYLOAD_TYPE: u8 = 102;
const OPUS_CLOCK_RATE: u32 = 48_000;
/// PT 111 = default Chrome/libwebrtc untuk Opus; memakai nilai yang sama
/// membuat pencocokan codec di answer libwebrtc exact (fmtp identik).
const OPUS_PAYLOAD_TYPE: u8 = 111;

/// Slot mid **per m-line** (index → a=mid), dibagikan ke handler ICE.
///
/// `to_json()` webrtc-rs hardcode `sdp_mid: ""` DAN `sdp_mline_index: 0`
/// (rtc 0.20.5). Dengan >1 m-line (video + audio), kandidat sender selalu
/// tampak berasal dari m-line 0; kita memetakan via index itu dan
/// mengandalkan BUNDLE libwebrtc (MAXBUNDLE) sehingga transport sama untuk
/// kedua m-line. Mid asli diisi dari SDP kita sendiri setelah local
/// description dibuat.
type MidSlot = Arc<StdMutex<Vec<Option<String>>>>;

/// Slot DataChannel masuk (sisi answerer).
type DcSlot = Arc<tokio::sync::Mutex<Option<Arc<dyn DataChannel>>>>;

/// Error ringkas peer (bungkus webrtc::error::Error sebagai string agar
/// Sync + mudah diteruskan lewat channel antar task).
#[derive(Debug)]
pub struct PeerError(pub String);

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "peer error: {}", self.0)
    }
}

impl std::error::Error for PeerError {}

impl From<webrtc::error::Error> for PeerError {
    fn from(e: webrtc::error::Error) -> Self {
        PeerError(e.to_string())
    }
}

pub type PeerResult<T> = Result<T, PeerError>;

/// Event keluar dari PeerConnection menuju lapisan signaling.
#[derive(Debug)]
pub enum PeerEvent {
    /// Kandidat ICE lokal hasil gathering (trickle).
    Ice(IceCandidate),
    /// DTLS/SCTP/channel siap (connection state Connected).
    Connected,
    /// DataChannel `ctrl` terbuka dan siap kirim.
    CtrlOpen,
    /// Pesan teks masuk di DataChannel `ctrl`.
    CtrlMessage(String),
}

/// Handler event webrtc-rs: meneruskan ke channel lapisan signaling.
#[derive(Clone)]
struct Handler {
    events: mpsc::UnboundedSender<PeerEvent>,
    /// Slot DC masuk (dipakai sisi answerer untuk membalas pesan).
    dc_slot: Option<DcSlot>,
    /// Mid m-line video (diisi setelah local description dibuat).
    mid: MidSlot,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let Ok(init) = event.candidate.to_json() else {
            return;
        };
        // ROOT CAUSE (T5→T6, diperluas R4): `RTCIceCandidate::to_json()` di
        // webrtc-rs 0.20.5 hardcode `sdp_mid: Some("")` dan
        // `sdp_mline_index: Some(0)` (rtc/.../transport/ice/candidate.rs:211).
        // libwebrtc menolak kandidat tanpa mid dikenal. Kita pakai index dari
        // event untuk mengambil mid asli dari SDP; karena index selalu 0,
        // kandidat dipetakan ke m-line 0 (video) dan diandalkan pada BUNDLE
        // libwebrtc (MAXBUNDLE) yang menyatukan transport video+audio.
        let idx = init.sdp_mline_index.unwrap_or_default() as usize;
        let sdp_mid = self
            .mid
            .lock()
            .ok()
            .and_then(|mids| mids.get(idx).cloned().flatten())
            .filter(|m| !m.is_empty())
            // Fallback terakhir: m-line pertama selalu ada di index 0.
            .unwrap_or_else(|| "0".to_string());
        let _ = self.events.send(PeerEvent::Ice(IceCandidate {
            candidate: init.candidate,
            sdp_mid,
            sdp_mline_index: init.sdp_mline_index.unwrap_or_default() as u32,
        }));
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if state == RTCPeerConnectionState::Connected {
            let _ = self.events.send(PeerEvent::Connected);
        }
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        // Sisi answerer: simpan DC agar bisa membalas + poll pesannya.
        if let Some(slot) = &self.dc_slot {
            *slot.lock().await = Some(dc.clone());
        }
        let events = self.events.clone();
        tokio::spawn(async move {
            while let Some(event) = dc.poll().await {
                if let DataChannelEvent::OnMessage(msg) = event
                    && let Ok(text) = String::from_utf8(msg.data.to_vec())
                {
                    let _ = events.send(PeerEvent::CtrlMessage(text));
                }
            }
        });
    }
}

/// Bangun PeerConnection dengan STUN + codec + handler event.
async fn build_pc(
    events: mpsc::UnboundedSender<PeerEvent>,
    dc_slot: Option<DcSlot>,
    media_engine: MediaEngine,
    mid: MidSlot,
) -> PeerResult<Arc<dyn PeerConnection>> {
    let config = RTCConfigurationBuilder::default()
        .with_ice_servers(vec![RTCIceServer {
            urls: vec![STUN_SERVER.to_owned()],
            ..Default::default()
        }])
        .build();

    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(media_engine)
        .with_handler(Arc::new(Handler {
            events,
            dc_slot,
            mid,
        }))
        .with_udp_addrs(vec!["0.0.0.0:0"])
        .build()
        .await?;
    Ok(Arc::new(pc))
}

/// Codec H.264 untuk negosiasi dengan libwebrtc (Android TV).
///
/// PENTING (temuan verifikasi T5 di device): libwebrtc memerlukan fmtp
/// H264 yang cocok dengan set decoder-nya. Tanpa `profile-level-id` dan
/// `packetization-mode`, libwebrtc mencatat "No video codecs in common"
/// lalu MENOLAK m=video di answer ("m= section '0' being rejected in
/// answer") → m-line tanpa transport → tanpa kandidat ICE → koneksi tak
/// pernah terbentuk. Nilai di bawah = default Chrome/libwebrtc.
fn h264_profile_level_id(profile: H264Profile) -> &'static str {
    // Level 4.2 (0x2a): aman untuk 1920x1080@30. Byte profile-iop 0xe0
    // menjaga Baseline tetap constrained; Main/High tidak memakai flags.
    match profile {
        H264Profile::Baseline => "42e02a",
        H264Profile::Main => "4d002a",
        H264Profile::High => "64002a",
    }
}

fn h264_codec(profile: H264Profile) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: H264_CLOCK_RATE,
            channels: 0,
            sdp_fmtp_line: format!(
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                h264_profile_level_id(profile)
            ),
            rtcp_feedback: vec![
                RTCPFeedback {
                    typ: "goog-remb".to_owned(),
                    parameter: String::new(),
                },
                RTCPFeedback {
                    typ: "transport-cc".to_owned(),
                    parameter: String::new(),
                },
                RTCPFeedback {
                    typ: "ccm".to_owned(),
                    parameter: "fir".to_owned(),
                },
                RTCPFeedback {
                    typ: "nack".to_owned(),
                    parameter: String::new(),
                },
                RTCPFeedback {
                    typ: "nack".to_owned(),
                    parameter: "pli".to_owned(),
                },
            ],
        },
        payload_type: H264_PAYLOAD_TYPE,
    }
}

/// Codec Opus untuk negosiasi audio dengan libwebrtc.
///
/// PT/fmtp mengikuti default Chrome/libwebrtc (`111`, `minptime=10;
/// useinbandfec=1`) agar libwebrtc memilih codec ini di answer tanpa
/// konflik. PT aktual ternegosiasi tetap diambil dari sender (lihat
/// [`SenderPeer::send_params`]).
fn opus_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: OPUS_CLOCK_RATE,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: OPUS_PAYLOAD_TYPE,
    }
}

/// Tambahkan video track H.264 (placeholder; data asli dari encoder T2
/// dikirim di T6).
async fn add_h264_placeholder(
    pc: &Arc<dyn PeerConnection>,
    codec: RTCRtpCodecParameters,
) -> PeerResult<Arc<TrackLocalStaticSample>> {
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "wdt-stream".to_owned(),
            "wdt-video".to_owned(),
            "wdt-video".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(fastrand::u32(..)),
                    ..Default::default()
                },
                codec: codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ),
    )?);
    pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
    Ok(track)
}

/// Tambahkan audio track Opus (sample ditulis pipeline audio R4).
async fn add_opus_track(
    pc: &Arc<dyn PeerConnection>,
    codec: RTCRtpCodecParameters,
) -> PeerResult<Arc<TrackLocalStaticSample>> {
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "wdt-stream".to_owned(),
            "wdt-audio".to_owned(),
            "wdt-audio".to_owned(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(fastrand::u32(..)),
                    ..Default::default()
                },
                codec: codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ),
    )?);
    pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
    Ok(track)
}

/// Konversi kandidat protokol → tipe webrtc-rs.
fn to_webrtc_candidate(c: &IceCandidate) -> RTCIceCandidateInit {
    RTCIceCandidateInit {
        candidate: c.candidate.clone(),
        sdp_mid: Some(c.sdp_mid.clone()),
        sdp_mline_index: Some(c.sdp_mline_index as u16),
        ..Default::default()
    }
}

/// Peer sisi sender (offerer).
pub struct SenderPeer {
    pub pc: Arc<dyn PeerConnection>,
    /// Mid per m-line hasil parsing SDP local (root-cause fix sdpMid).
    mid: MidSlot,
    /// Track video (holder sample H.264 ditulis di sini).
    video_track: Arc<TrackLocalStaticSample>,
    /// Track audio Opus (None bila sesi video-only).
    audio_track: Option<Arc<TrackLocalStaticSample>>,
    /// DataChannel `ctrl` (opsional). Jalur video produksi tidak memakai
    /// DataChannel: offer yang hanya berisi m=video lebih interoperable
    /// dengan libwebrtc.
    dc: Option<Arc<dyn DataChannel>>,
    events: mpsc::UnboundedReceiver<PeerEvent>,
}

impl SenderPeer {
    /// Peer dengan DataChannel `ctrl` (dipakai test T3: ping/pong).
    pub async fn new() -> PeerResult<Self> {
        Self::new_inner(true, H264Profile::Baseline, false).await
    }

    /// Peer dengan DataChannel dan profile H.264 eksplisit untuk A/B.
    pub async fn with_ctrl_profile(profile: H264Profile) -> PeerResult<Self> {
        Self::new_inner(true, profile, false).await
    }

    /// Peer khusus video (tanpa DataChannel) — bentuk jalur produksi (T4/T6).
    pub async fn video_only() -> PeerResult<Self> {
        Self::new_inner(false, H264Profile::Baseline, false).await
    }

    /// Peer video-only dengan profile H.264 eksplisit untuk A/B.
    pub async fn video_only_with_profile(profile: H264Profile) -> PeerResult<Self> {
        Self::new_inner(false, profile, false).await
    }

    /// Peer audio+video (tanpa DataChannel) — jalur produksi R4 ketika
    /// receiver mendukung audio. m=audio selalu dinegosiasikan; apakah sample
    /// benar-benar dikirim ditentukan route (lihat `sender_session`).
    pub async fn av_with_profile(profile: H264Profile) -> PeerResult<Self> {
        Self::new_inner(false, profile, true).await
    }

    async fn new_inner(
        with_ctrl: bool,
        profile: H264Profile,
        with_audio: bool,
    ) -> PeerResult<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut media_engine = MediaEngine::default();
        // SENGAJA tidak `register_default_codecs()`: dedup MediaEngine
        // berdasarkan (mime, payload_type) membuat H264 default (fmtp
        // profile-level-id=42e01f) MENANG dan menimpa fmtp level 4.2 kita —
        // terverifikasi via examples/print_offer. Kita daftarkan codec
        // secara eksplisit: H264 (selalu) + Opus (bila audio).
        let codec = h264_codec(profile);
        media_engine.register_codec(codec.clone(), RtpCodecKind::Video)?;
        let opus = if with_audio {
            let opus = opus_codec();
            media_engine.register_codec(opus.clone(), RtpCodecKind::Audio)?;
            Some(opus)
        } else {
            None
        };
        let mid: MidSlot = Arc::new(StdMutex::new(Vec::new()));
        let pc = build_pc(tx.clone(), None, media_engine, mid.clone()).await?;
        let dc = if with_ctrl {
            let dc = pc
                .create_data_channel(
                    CTRL_LABEL,
                    Some(RTCDataChannelInit {
                        ordered: true,
                        ..Default::default()
                    }),
                )
                .await?;
            // Poll DC sendiri di background: teruskan OnOpen/OnMessage.
            let dc_poll = dc.clone();
            tokio::spawn(async move {
                let mut open_sent = false;
                while let Some(event) = dc_poll.poll().await {
                    match event {
                        DataChannelEvent::OnOpen => {
                            if !open_sent {
                                open_sent = true;
                                let _ = tx.send(PeerEvent::CtrlOpen);
                            }
                        }
                        DataChannelEvent::OnMessage(msg) => {
                            if let Ok(text) = String::from_utf8(msg.data.to_vec()) {
                                let _ = tx.send(PeerEvent::CtrlMessage(text));
                            }
                        }
                        DataChannelEvent::OnClose => break,
                        _ => {}
                    }
                }
            });
            Some(dc)
        } else {
            None
        };
        // Urutan penting: video dulu (m-line 0), audio kedua (m-line 1).
        let video_track = add_h264_placeholder(&pc, codec).await?;
        let audio_track = match opus {
            Some(c) => Some(add_opus_track(&pc, c).await?),
            None => None,
        };
        Ok(Self {
            pc,
            mid,
            video_track,
            audio_track,
            dc,
            events: rx,
        })
    }

    /// Track video tempat sample H.264 ditulis (dipakai pipeline T6).
    pub fn video_track(&self) -> Arc<TrackLocalStaticSample> {
        self.video_track.clone()
    }

    /// Track audio tempat sample Opus ditulis (None bila video-only).
    pub fn audio_track(&self) -> Option<Arc<TrackLocalStaticSample>> {
        self.audio_track.clone()
    }

    /// SSRC + payload type **ternegosiasi** untuk media `kind`.
    ///
    /// `write_sample` menstempel PT dari argumen dan webrtc-rs dapat
    /// menomori ulang PT di SDP (mis. 102 → 125), jadi PT wajib diambil
    /// dari parameter sender yang **cocok dengan media kind** — bukan
    /// asumsi "sender pertama = video" yang rusak setelah ada audio track.
    pub async fn send_params(&self, kind: RtpCodecKind) -> Option<(SSRC, PayloadType)> {
        let track = match kind {
            RtpCodecKind::Video => &self.video_track,
            RtpCodecKind::Audio => self.audio_track.as_ref()?,
            _ => return None,
        };
        let ssrc = *track.ssrcs().await.first()?;
        for sender in self.pc.get_senders().await {
            if sender.track().kind().await != kind {
                continue;
            }
            let payload_type = sender
                .get_parameters()
                .await
                .ok()?
                .rtp_parameters
                .codecs
                .first()?
                .payload_type;
            return Some((ssrc, payload_type));
        }
        None
    }

    /// SSRC + payload type ternegosiasi untuk m-line video (kompat).
    pub async fn video_send_params(&self) -> Option<(SSRC, PayloadType)> {
        self.send_params(RtpCodecKind::Video).await
    }

    /// SSRC + payload type ternegosiasi untuk m-line audio.
    pub async fn audio_send_params(&self) -> Option<(SSRC, PayloadType)> {
        self.send_params(RtpCodecKind::Audio).await
    }

    /// Mid m-line video yang dipakai untuk kandidat ICE.
    ///
    /// None sebelum local description dibuat. Dipakai test untuk
    /// membuktikan mid berasal dari SDP (bukan fallback hardcode).
    pub fn video_mid(&self) -> Option<String> {
        self.mid
            .lock()
            .ok()
            .and_then(|m| m.first().cloned().flatten())
    }

    /// Mid m-line audio (None bila video-only / belum dibuat).
    pub fn audio_mid(&self) -> Option<String> {
        self.mid
            .lock()
            .ok()
            .and_then(|m| m.get(1).cloned().flatten())
    }

    /// Tunggu sampai DataChannel `ctrl` terbuka (siap kirim).
    pub async fn wait_ctrl_open(&mut self, timeout: Duration) -> PeerResult<()> {
        if self.dc.is_none() {
            return Ok(()); // video-only: tidak ada DC untuk ditunggu
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.next_event(Duration::from_secs(2)).await {
                Ok(PeerEvent::CtrlOpen) => return Ok(()),
                Ok(_) => {}
                Err(_) => {
                    if tokio::time::Instant::now() > deadline {
                        return Err(PeerError("timeout menunggu ctrl open".into()));
                    }
                }
            }
        }
    }

    /// Buat SDP offer (string) setelah set local description.
    ///
    /// Sekaligus menyimpan mid m-line video dari SDP untuk dipakai handler
    /// ICE (root-cause fix: `to_json()` hardcode mid kosong).
    pub async fn create_offer(&self) -> PeerResult<String> {
        let offer = self.pc.create_offer(None).await?;
        let sdp = offer.sdp.clone();
        self.pc.set_local_description(offer).await?;
        let mids: Vec<Option<String>> = sdp_util::parse_sections(&sdp)
            .into_iter()
            .map(|s| s.mid)
            .collect();
        if let Ok(mut slot) = self.mid.lock() {
            *slot = mids;
        }
        Ok(sdp)
    }

    pub async fn set_answer(&self, sdp: &str) -> PeerResult<()> {
        let desc = RTCSessionDescription::answer(sdp.to_owned())
            .map_err(|e| PeerError(format!("answer SDP tidak valid: {e}")))?;
        self.pc.set_remote_description(desc).await?;
        Ok(())
    }

    pub async fn add_ice(&self, c: &IceCandidate) -> PeerResult<()> {
        self.pc.add_ice_candidate(to_webrtc_candidate(c)).await?;
        Ok(())
    }

    pub async fn send_ctrl(&self, text: &str) -> PeerResult<()> {
        match &self.dc {
            Some(dc) => {
                dc.send_text(text).await?;
                Ok(())
            }
            None => Err(PeerError("peer ini tanpa DataChannel (video-only)".into())),
        }
    }

    /// Ambil event berikutnya (timeout → Err).
    pub async fn next_event(&mut self, timeout: Duration) -> PeerResult<PeerEvent> {
        tokio::time::timeout(timeout, self.events.recv())
            .await
            .map_err(|_| PeerError("timeout menunggu peer event".into()))?
            .ok_or_else(|| PeerError("event channel tertutup".into()))
    }
}

/// Peer sisi receiver (answerer): tanpa track lokal; menunggu DataChannel.
pub struct ReceiverPeer {
    pub pc: Arc<dyn PeerConnection>,
    /// Mid m-line video hasil parsing SDP answer (root-cause fix sdpMid).
    mid: MidSlot,
    events: mpsc::UnboundedReceiver<PeerEvent>,
    incoming_dc: DcSlot,
}

impl ReceiverPeer {
    /// Receiver **video-only** (tanpa Opus) — merepresentasikan receiver
    /// LAMA di test: m=audio pada offer akan ditolak di answer.
    pub async fn new() -> PeerResult<Self> {
        Self::new_inner(false).await
    }

    /// Receiver audio+video (mendaftarkan Opus) — merepresentasikan receiver
    /// R4 di test in-process.
    pub async fn with_audio() -> PeerResult<Self> {
        Self::new_inner(true).await
    }

    async fn new_inner(with_audio: bool) -> PeerResult<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let incoming_dc = Arc::new(tokio::sync::Mutex::new(None));
        let mid: MidSlot = Arc::new(StdMutex::new(Vec::new()));
        let mut media_engine = MediaEngine::default();
        // Hanya codec eksplisit (lihat catatan di SenderPeer::new_inner).
        media_engine.register_codec(h264_codec(H264Profile::Baseline), RtpCodecKind::Video)?;
        if with_audio {
            media_engine.register_codec(opus_codec(), RtpCodecKind::Audio)?;
        }
        let pc = build_pc(tx, Some(incoming_dc.clone()), media_engine, mid.clone()).await?;
        Ok(Self {
            pc,
            mid,
            events: rx,
            incoming_dc,
        })
    }

    /// Kirim teks lewat DataChannel masuk (menunggu sampai tersedia).
    pub async fn send_ctrl(&self, text: &str) -> PeerResult<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(dc) = self.incoming_dc.lock().await.as_ref() {
                dc.send_text(text).await?;
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(PeerError("data channel masuk tidak kunjung tiba".into()));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Set remote offer → kembalikan SDP answer (string).
    pub async fn set_offer(&self, sdp: &str) -> PeerResult<String> {
        let desc = RTCSessionDescription::offer(sdp.to_owned())
            .map_err(|e| PeerError(format!("offer SDP tidak valid: {e}")))?;
        self.pc.set_remote_description(desc).await?;
        let answer = self.pc.create_answer(None).await?;
        let out = answer.sdp.clone();
        self.pc.set_local_description(answer).await?;
        let mids: Vec<Option<String>> = sdp_util::parse_sections(&out)
            .into_iter()
            .map(|s| s.mid)
            .collect();
        if let Ok(mut slot) = self.mid.lock() {
            *slot = mids;
        }
        Ok(out)
    }

    pub async fn add_ice(&self, c: &IceCandidate) -> PeerResult<()> {
        self.pc.add_ice_candidate(to_webrtc_candidate(c)).await?;
        Ok(())
    }

    pub async fn next_event(&mut self, timeout: Duration) -> PeerResult<PeerEvent> {
        tokio::time::timeout(timeout, self.events.recv())
            .await
            .map_err(|_| PeerError("timeout menunggu peer event".into()))?
            .ok_or_else(|| PeerError("event channel tertutup".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_profiles_map_to_matching_level_42_fmtp() {
        assert_eq!(h264_profile_level_id(H264Profile::Baseline), "42e02a");
        assert_eq!(h264_profile_level_id(H264Profile::Main), "4d002a");
        assert_eq!(h264_profile_level_id(H264Profile::High), "64002a");
        for profile in [H264Profile::Baseline, H264Profile::Main, H264Profile::High] {
            assert!(
                h264_codec(profile)
                    .rtp_codec
                    .sdp_fmtp_line
                    .contains(h264_profile_level_id(profile))
            );
        }
    }

    /// Root-cause fix bug #5: mid kandidat ICE harus berasal dari SDP kita,
    /// bukan dari `to_json()` (yang hardcode kosong) maupun fallback "0".
    /// Test ini gagal bila create_offer tidak mengisi slot mid.
    #[tokio::test]
    async fn ice_mid_comes_from_sdp() {
        let peer = SenderPeer::video_only().await.expect("peer");
        assert_eq!(
            peer.video_mid(),
            None,
            "slot mid harus kosong sebelum offer"
        );

        let sdp = peer.create_offer().await.expect("offer");
        let parsed = sdp_util::parse_video_section(&sdp);

        assert!(parsed.mid.is_some(), "offer harus punya a=mid pada m=video");
        assert_eq!(
            peer.video_mid(),
            parsed.mid,
            "mid ICE harus sama dengan a=mid di SDP"
        );
    }

    /// Gate T3.3: dua PC in-process handshake langsung (tanpa server) —
    /// offer/answer + trickle ICE + DataChannel ping/pong.
    #[tokio::test]
    async fn two_peers_connect_in_process() {
        let mut sender = SenderPeer::new().await.expect("sender pc");
        let mut receiver = ReceiverPeer::new().await.expect("receiver pc");

        let offer = sender.create_offer().await.expect("offer");
        assert!(offer.contains("v=0"), "offer harus SDP valid");
        // Regresi interop: fmtp H264 harus punya profile-level-id agar
        // libwebrtc bisa mencocokkan codec.
        assert!(
            offer.contains("profile-level-id"),
            "offer harus memuat profile-level-id H264: {offer}"
        );
        let answer = receiver.set_offer(&offer).await.expect("answer");
        assert!(answer.contains("v=0"), "answer harus SDP valid");
        sender.set_answer(&answer).await.expect("set answer");

        let timeout = Duration::from_secs(15);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut sender_on = false;
        let mut receiver_on = false;
        let mut got_pong = false;
        let mut ping_sent = false;
        loop {
            if sender_on && receiver_on && got_pong {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "timeout handshake: sender={sender_on} receiver={receiver_on} pong={got_pong}"
                );
            }
            tokio::select! {
                ev = sender.next_event(Duration::from_secs(2)) => {
                    match ev.expect("sender event") {
                        PeerEvent::Ice(c) => { receiver.add_ice(&c).await.expect("add ice"); }
                        PeerEvent::Connected => sender_on = true,
                        PeerEvent::CtrlOpen => {
                            if !ping_sent {
                                ping_sent = true;
                                sender.send_ctrl("ping").await.expect("send ping");
                            }
                        }
                        PeerEvent::CtrlMessage(m) => {
                            if m == "pong" { got_pong = true; }
                        }
                    }
                }
                ev = receiver.next_event(Duration::from_secs(2)) => {
                    match ev.expect("receiver event") {
                        PeerEvent::Ice(c) => { sender.add_ice(&c).await.expect("add ice"); }
                        PeerEvent::Connected => receiver_on = true,
                        PeerEvent::CtrlOpen => {}
                        PeerEvent::CtrlMessage(m) => {
                            if m == "ping" {
                                receiver.send_ctrl("pong").await.expect("send pong");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Offer audio+video: dua m-line, fmtp H264 tetap utuh, Opus hadir.
    #[tokio::test]
    async fn av_offer_has_video_and_audio_m_lines() {
        let peer = SenderPeer::av_with_profile(H264Profile::Baseline)
            .await
            .expect("peer av");
        assert!(
            peer.audio_track().is_some(),
            "peer AV harus punya audio track"
        );
        let sdp = peer.create_offer().await.expect("offer");

        let sections = sdp_util::parse_sections(&sdp);
        assert_eq!(sections.len(), 2, "harus ada 2 m-line: {sections:?}");
        assert_eq!(sections[0].kind, "video");
        assert_eq!(sections[1].kind, "audio");
        // H264 tetap dengan fmtp kustom.
        assert!(
            sdp.contains("profile-level-id=42e02a"),
            "fmtp H264 tidak boleh rusak: {sdp}"
        );
        // Opus teriklankan.
        assert!(
            sdp.contains("opus/48000/2"),
            "offer harus memuat opus/48000/2: {sdp}"
        );
        // mid video & audio terisi dari SDP.
        assert_eq!(peer.video_mid().as_deref(), sections[0].mid.as_deref());
        assert_eq!(peer.audio_mid().as_deref(), sections[1].mid.as_deref());
        assert_ne!(peer.video_mid(), peer.audio_mid());
    }

    /// PT/SSRC dipilih per media kind, bukan sender pertama.
    #[tokio::test]
    async fn send_params_resolve_by_media_kind() {
        let peer = SenderPeer::av_with_profile(H264Profile::Baseline)
            .await
            .expect("peer av");
        peer.create_offer().await.expect("offer");

        let (v_ssrc, v_pt) = peer.video_send_params().await.expect("video params");
        let (a_ssrc, a_pt) = peer.audio_send_params().await.expect("audio params");

        assert_ne!(v_ssrc, a_ssrc, "SSRC video/audio harus berbeda");
        assert_ne!(v_pt, a_pt, "PT video/audio harus berbeda");
        // PT video harus H264 (102 atau renumber); PT audio Opus (111 atau renumber).
        assert!(v_pt != a_pt);
    }

    /// Receiver video-only (lama) menolak m=audio: answer hanya punya
    /// m=video, sehingga jalur lama tetap aman.
    #[tokio::test]
    async fn legacy_receiver_rejects_audio_mline() {
        let sender = SenderPeer::av_with_profile(H264Profile::Baseline)
            .await
            .expect("sender av");
        let receiver = ReceiverPeer::new().await.expect("receiver video-only");
        let offer = sender.create_offer().await.expect("offer");
        let answer = receiver.set_offer(&offer).await.expect("answer");

        let a_sections = sdp_util::parse_sections(&answer);
        let video = a_sections.iter().find(|s| s.kind == "video");
        assert!(video.is_some(), "answer harus punya m=video");
        assert!(
            !answer.contains("opus/48000"),
            "receiver video-only tidak boleh menerima Opus: {answer}"
        );
    }

    /// Handshake AV in-process: koneksi terbentuk dengan audio answer.
    #[tokio::test]
    async fn av_two_peers_connect_in_process() {
        let mut sender = SenderPeer::av_with_profile(H264Profile::Baseline)
            .await
            .expect("sender");
        let mut receiver = ReceiverPeer::with_audio().await.expect("receiver");

        let offer = sender.create_offer().await.expect("offer");
        let answer = receiver.set_offer(&offer).await.expect("answer");
        assert!(
            answer.contains("opus/48000"),
            "receiver audio harus menerima Opus: {answer}"
        );
        sender.set_answer(&answer).await.expect("set answer");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut connected = false;
        while !connected {
            if tokio::time::Instant::now() > deadline {
                panic!("timeout AV handshake");
            }
            tokio::select! {
                ev = sender.next_event(Duration::from_secs(2)) => {
                    match ev.expect("sender event") {
                        PeerEvent::Ice(c) => { receiver.add_ice(&c).await.expect("add ice"); }
                        PeerEvent::Connected => connected = true,
                        _ => {}
                    }
                }
                ev = receiver.next_event(Duration::from_secs(2)) => {
                    match ev.expect("receiver event") {
                        PeerEvent::Ice(c) => { sender.add_ice(&c).await.expect("add ice"); }
                        _ => {}
                    }
                }
            }
        }
        // Params kedua media ternegosiasi.
        assert!(sender.video_send_params().await.is_some());
        assert!(sender.audio_send_params().await.is_some());
    }
}
