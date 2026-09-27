package com.wdt.receiver

import com.wdt.receiver.device.DeviceCapability
import com.wdt.receiver.device.DeviceTier
import com.wdt.receiver.pairing.ManualPairing
import com.wdt.receiver.pairing.PairingResult
import com.wdt.receiver.signaling.AnswerOut
import com.wdt.receiver.signaling.HelloOut
import com.wdt.receiver.signaling.IceCandidateDto
import com.wdt.receiver.signaling.IceOut
import com.wdt.receiver.signaling.ServerMsg
import com.wdt.receiver.signaling.ServerMsgParser
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Verifikasi kontrak signaling (tanpa device).
 *
 * Casing field WAJIB sama dengan `docs/SIGNALING_PROTOCOL.md` dan
 * `core/src/signaling/protocol.rs` (proto v1).
 */
class ProtocolContractTest {

    @Test
    fun hello_matches_contract_casing() {
        // Cermin test Rust wire_format_stable (protocol.rs). Receiver R4
        // mengiklankan caps audio.
        val json = ServerMsgParser.encode(
            HelloOut(token = "123456", deviceId = "tv-living"),
        )
        assertEquals(
            """{"type":"hello","role":"receiver","proto":1,"token":"123456","deviceId":"tv-living","caps":{"audio":true}}""",
            json,
        )
    }

    @Test
    fun parse_session_config() {
        val msg = ServerMsgParser.parse(
            """{"type":"sessionConfig","audio":{"enabled":true,"route":"tv","channels":2,"sampleRate":48000,"codec":"opus"}}""",
        )
        val cfg = (msg as ServerMsg.SessionConfig).audio
        assertTrue(cfg.enabled)
        assertEquals("tv", cfg.route)
        assertEquals(2, cfg.channels)
        assertEquals(48000, cfg.sampleRate)
        assertEquals("opus", cfg.codec)
    }

    @Test
    fun session_config_disabled_defaults_when_absent() {
        // Tanpa field audio → default aman (tidak aktif).
        val msg = ServerMsgParser.parse("""{"type":"sessionConfig"}""")
        val cfg = (msg as ServerMsg.SessionConfig).audio
        assertEquals(false, cfg.enabled)
        assertEquals("laptop", cfg.route)
    }

    @Test
    fun ice_out_uses_sdpMid_and_sdpMLineIndex() {
        val json = ServerMsgParser.encode(
            IceOut(
                candidate = IceCandidateDto(
                    candidate = "candidate:1 udp 1 192.168.1.5 50000 typ host",
                    sdpMid = "0",
                    sdpMLineIndex = 0,
                ),
            ),
        )
        assertTrue("harus ada sdpMid: $json", json.contains("\"sdpMid\":\"0\""))
        assertTrue("harus ada sdpMLineIndex: $json", json.contains("\"sdpMLineIndex\":0"))
        assertTrue(json.contains("\"type\":\"ice\""))
    }

    @Test
    fun answer_out_shape() {
        val json = ServerMsgParser.encode(
            AnswerOut(sdp = "v=0 TEST"),
        )
        assertEquals("""{"type":"answer","sdp":"v=0 TEST"}""", json)
    }

    @Test
    fun parse_helloOk() {
        val msg = ServerMsgParser.parse("""{"type":"helloOk","proto":1,"server":"wdt/0.1.0"}""")
        assertEquals(ServerMsg.HelloOk(1, "wdt/0.1.0"), msg)
    }

    @Test
    fun parse_offer() {
        val msg = ServerMsgParser.parse("""{"type":"offer","sdp":"v=0 OFFER"}""")
        assertEquals(ServerMsg.Offer("v=0 OFFER"), msg)
    }

    @Test
    fun parse_ice_relay() {
        val msg = ServerMsgParser.parse(
            """{"type":"ice","candidate":{"candidate":"candidate:1 udp 1 10.0.0.1 9999 typ host","sdpMid":"0","sdpMLineIndex":0}}""",
        )
        val ice = msg as ServerMsg.Ice
        assertEquals("0", ice.candidate.sdpMid)
        assertEquals(0, ice.candidate.sdpMLineIndex)
    }

    @Test
    fun parse_error_and_bye() {
        assertEquals(
            ServerMsg.ErrorMsg("badToken", "token pairing salah"),
            ServerMsgParser.parse("""{"type":"error","code":"badToken","message":"token pairing salah"}"""),
        )
        assertEquals(
            ServerMsg.Bye("peerDisconnected"),
            ServerMsgParser.parse("""{"type":"bye","reason":"peerDisconnected"}"""),
        )
    }

    @Test
    fun unknown_type_is_tolerated() {
        // receiverJoined hanya untuk sender; receiver harus mengabaikan, bukan crash.
        val msg = ServerMsgParser.parse("""{"type":"receiverJoined","deviceId":"tv"}""")
        assertEquals(ServerMsg.Unknown("receiverJoined"), msg)
    }

    @Test
    fun invalid_json_returns_null() {
        assertEquals(null, ServerMsgParser.parse("bukan json"))
        assertEquals(null, ServerMsgParser.parse("""{"tanpa":"type"}"""))
    }

    @Test
    fun manual_pairing_accepts_documented_format() {
        val result = ManualPairing.parse(" 192.168.1.5:8420:123456 ")
        assertTrue(result is PairingResult.Success)
        val info = (result as PairingResult.Success).info
        assertEquals("192.168.1.5", info.host)
        assertEquals(8420, info.port)
        assertEquals("123456", info.token)
        assertEquals("ws://192.168.1.5:8420/ws", info.wsUrl())
    }

    @Test
    fun manual_pairing_accepts_token_only_for_discovered_sender() {
        // Discovery NSD sudah memberi host:port → user cukup ketik token.
        assertEquals("439769", ManualPairing.parseTokenOnly(" 439769 "))
        assertEquals(null, ManualPairing.parseTokenOnly("43976"))
        assertEquals(null, ManualPairing.parseTokenOnly("43976a"))
        assertEquals(null, ManualPairing.parseTokenOnly(""))
    }

    @Test
    fun manual_pairing_rejects_bad_input() {
        assertTrue(ManualPairing.parse("") is PairingResult.Failure)
        assertTrue(ManualPairing.parse("192.168.1.5:8420") is PairingResult.Failure)
        assertTrue(ManualPairing.parse("192.168.1.5:0:123456") is PairingResult.Failure)
        assertTrue(ManualPairing.parse("192.168.1.5:8420:12ab56") is PairingResult.Failure)
        assertTrue(ManualPairing.parse("999.999.999.999:8420:123456") is PairingResult.Failure)
    }

    @Test
    fun tier_thresholds_match_agreement() {        // low bila tanpa HW decoder, apa pun RAM-nya.
        assertEquals(DeviceTier.LOW, DeviceCapability.tierFor(8_000, hwH264 = false))
        // batas RAM 2.5 GB.
        assertEquals(DeviceTier.LOW, DeviceCapability.tierFor(2_500, hwH264 = true))
        assertEquals(DeviceTier.MID, DeviceCapability.tierFor(2_501, hwH264 = true))
        // batas RAM 4 GB.
        assertEquals(DeviceTier.MID, DeviceCapability.tierFor(4_000, hwH264 = true))
        assertEquals(DeviceTier.HIGH, DeviceCapability.tierFor(4_001, hwH264 = true))
        // Xiaomi TV baseline ~2GB + HW decoder → LOW.
        assertEquals(DeviceTier.LOW, DeviceCapability.tierFor(2_000, hwH264 = true))
    }

    @Test
    fun hardware_name_heuristic_for_api_below_29() {
        // Terverifikasi dari device uji (MiTV-MOOR2): hardware vendor MStar.
        assertTrue(DeviceCapability.isLikelyHardwareByName("OMX.MS.AVC.Decoder"))
        // Software AOSP (codec2 & OMX Google) harus dikenali sebagai software.
        assertTrue(!DeviceCapability.isLikelyHardwareByName("c2.android.avc.decoder"))
        assertTrue(!DeviceCapability.isLikelyHardwareByName("OMX.google.h264.decoder"))
    }
}
