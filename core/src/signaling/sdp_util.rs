//! Parsing SDP minimal untuk kebutuhan pipelines WebRTC kita.
//!
//! Dipakai karena dua keterbatasan webrtc-rs 0.20 yang ditemukan saat
//! verifikasi device (T5/T6):
//! 1. `RTCIceCandidate::to_json()` **hardcode** `sdp_mid: Some("")`
//!    (`rtc-0.20.5/src/peer_connection/transport/ice/candidate.rs:211`),
//!    sehingga kandidat ICE yang kita relay tidak punya mid yang benar.
//!    libwebrtc menolaknya ("Not adding candidate because the JsepTransport
//!    doesn't exist") → ICE tak pernah selesai. Mid asli harus diambil dari
//!    SDP kita sendiri.
//! 2. `write_sample` menstempel payload type dari argumen, dan webrtc-rs
//!    dapat menomori ulang PT di SDP (mis. 102 → 125). PT yang dipakai di
//!    wire harus PT ternegosiasi dari local description.

/// Bagian `m=video` pertama pada sebuah SDP.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VideoSection {
    /// Nilai `a=mid:` pada m-line video.
    pub mid: Option<String>,
    /// Payload type yang diiklankan pada baris `m=video`.
    pub payload_types: Vec<u8>,
    /// SSRC pertama dari `a=ssrc:<id>` pada seksi video.
    pub ssrc: Option<u32>,
}

/// Satu m-line SDP (video/audio/application) beserta atributnya.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSection {
    /// Indeks m-line (0-based, urutan kemunculan).
    pub index: usize,
    /// Jenis media: `"video"`, `"audio"`, `"application"`, dsb.
    pub kind: String,
    /// Nilai `a=mid:` (BUNDLE id).
    pub mid: Option<String>,
    /// Payload type pada baris `m=`.
    pub payload_types: Vec<u8>,
    /// SSRC pertama `a=ssrc:<id>` (bila ada).
    pub ssrc: Option<u32>,
}

/// Parse **semua** m-line SDP (urutan penting: indeks = sdpMLineIndex).
///
/// Dipakai untuk memetakan kandidat ICE ke mid per m-line saat sesi punya
/// lebih dari satu media (video + audio).
pub fn parse_sections(sdp: &str) -> Vec<MediaSection> {
    let mut out: Vec<MediaSection> = Vec::new();
    for raw in sdp.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix("m=") {
            let mut fields = rest.split_whitespace();
            let kind = fields.next().unwrap_or("").to_string();
            let _port = fields.next();
            let _proto = fields.next();
            let payload_types = fields.filter_map(|f| f.parse::<u8>().ok()).collect();
            out.push(MediaSection {
                index: out.len(),
                kind,
                mid: None,
                payload_types,
                ssrc: None,
            });
            continue;
        }
        let Some(cur) = out.last_mut() else {
            continue;
        };
        if let Some(v) = line.strip_prefix("a=mid:") {
            cur.mid = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("a=ssrc:") {
            if cur.ssrc.is_none() {
                cur.ssrc = v.split_whitespace().next().and_then(|id| id.parse().ok());
            }
        }
    }
    out
}

impl VideoSection {
    /// Payload type pertama (yang dipakai untuk menulis sample).
    pub fn primary_payload_type(&self) -> Option<u8> {
        self.payload_types.first().copied()
    }
}

/// Parse seksi `m=video` pertama dari SDP (baris `\r\n` atau `\n`).
pub fn parse_video_section(sdp: &str) -> VideoSection {
    let mut out = VideoSection::default();
    let mut in_video = false;

    for raw in sdp.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);

        if let Some(rest) = line.strip_prefix("m=") {
            in_video = rest.starts_with("video ");
            if in_video {
                // m=video <port> <proto> <fmt> [fmt...]
                let mut fields = rest.split_whitespace();
                let _media = fields.next();
                let _port = fields.next();
                let _proto = fields.next();
                out.payload_types = fields.filter_map(|f| f.parse::<u8>().ok()).collect();
            }
            continue;
        }

        if !in_video {
            continue;
        }

        if let Some(v) = line.strip_prefix("a=mid:") {
            out.mid = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("a=ssrc:") {
            // "a=ssrc:<id> cname:..." — catat yang pertama saja.
            if out.ssrc.is_none() {
                out.ssrc = v.split_whitespace().next().and_then(|id| id.parse().ok());
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO_ONLY: &str = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 125\r\n\
c=IN IP4 0.0.0.0\r\n\
a=mid:0\r\n\
a=rtcp-mux\r\n\
a=ssrc-group:FID 1234 5678\r\n\
a=ssrc:1234 cname:wdt-stream\r\n\
a=ssrc:1234 msid:wdt-stream wdt-video\r\n";

    const VIDEO_PLUS_APP: &str = "v=0\r\n\
a=group:BUNDLE 0 1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
a=mid:0\r\n\
a=ssrc:11 cname:x\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
a=mid:1\r\n\
a=sctp-port:5000\r\n";

    #[test]
    fn parses_video_only_offer() {
        let v = parse_video_section(VIDEO_ONLY);
        assert_eq!(v.mid.as_deref(), Some("0"));
        assert_eq!(v.primary_payload_type(), Some(125));
        assert_eq!(v.ssrc, Some(1234)); // bukan 5678 (ssrc-group diabaikan)
    }

    #[test]
    fn stops_at_next_media_section() {
        // Seksi application tidak boleh mengubah hasil seksi video.
        let v = parse_video_section(VIDEO_PLUS_APP);
        assert_eq!(v.mid.as_deref(), Some("0"));
        assert_eq!(v.primary_payload_type(), Some(102));
        assert_eq!(v.ssrc, Some(11));
    }

    #[test]
    fn handles_missing_video_section() {
        let v = parse_video_section("v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n");
        assert_eq!(v, VideoSection::default());
    }

    #[test]
    fn tolerates_lf_only_line_endings() {
        let sdp = VIDEO_ONLY.replace("\r\n", "\n");
        let v = parse_video_section(&sdp);
        assert_eq!(v.mid.as_deref(), Some("0"));
        assert_eq!(v.ssrc, Some(1234));
    }

    const VIDEO_PLUS_AUDIO: &str = "v=0\r\n\
a=group:BUNDLE 0 1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
a=mid:0\r\n\
a=ssrc:11 cname:x\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:1\r\n\
a=ssrc:22 cname:x\r\n\
a=rtpmap:111 opus/48000/2\r\n";

    #[test]
    fn parses_all_media_sections_in_order() {
        let sections = parse_sections(VIDEO_PLUS_AUDIO);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].kind, "video");
        assert_eq!(sections[0].index, 0);
        assert_eq!(sections[0].mid.as_deref(), Some("0"));
        assert_eq!(sections[0].payload_types, vec![102]);
        assert_eq!(sections[0].ssrc, Some(11));
        assert_eq!(sections[1].kind, "audio");
        assert_eq!(sections[1].index, 1);
        assert_eq!(sections[1].mid.as_deref(), Some("1"));
        assert_eq!(sections[1].payload_types, vec![111]);
        assert_eq!(sections[1].ssrc, Some(22));
    }

    #[test]
    fn parse_sections_ignores_session_level_mid_scope() {
        // Verifikasi ssrc per seksi tidak tertukar.
        let sections = parse_sections(VIDEO_PLUS_APP);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].mid.as_deref(), Some("0"));
        assert_eq!(sections[1].kind, "application");
        assert_eq!(sections[1].mid.as_deref(), Some("1"));
    }
}
