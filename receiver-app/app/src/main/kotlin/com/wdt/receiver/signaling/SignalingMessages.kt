package com.wdt.receiver.signaling

import com.google.gson.GsonBuilder
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.google.gson.annotations.SerializedName

/**
 * Konstanta + DTO protokol signaling.
 *
 * Single source of truth di Rust: `core/src/signaling/protocol.rs`;
 * dokumen kontrak: `docs/SIGNALING_PROTOCOL.md`. Jangan ubah casing di sini
 * tanpa mengubah keduanya (proto v1).
 */
object Protocol {
    /** PROTO_VERSION di protocol.rs. */
    const val VERSION = 1

    /** WS_PATH di protocol.rs. */
    const val WS_PATH = "/ws"

    /** DEFAULT_PORT di protocol.rs. */
    const val DEFAULT_PORT = 8420

    /** MDNS_SERVICE_TYPE di protocol.rs (bentuk Android tanpa domain). */
    const val MDNS_SERVICE_TYPE = "_wdt._tcp"

    const val ROLE_RECEIVER = "receiver"

    const val TYPE_HELLO = "hello"
    const val TYPE_HELLO_OK = "helloOk"
    const val TYPE_OFFER = "offer"
    const val TYPE_ANSWER = "answer"
    const val TYPE_ICE = "ice"
    const val TYPE_SESSION_CONFIG = "sessionConfig"
    const val TYPE_ERROR = "error"
    const val TYPE_BYE = "bye"
}

/**
 * Kandidat ICE. Casing field WAJIB persis `candidate`/`sdpMid`/`sdpMLineIndex`
 * sesuai docs/SIGNALING_PROTOCOL.md.
 */
data class IceCandidateDto(
    @SerializedName("candidate") val candidate: String,
    @SerializedName("sdpMid") val sdpMid: String,
    @SerializedName("sdpMLineIndex") val sdpMLineIndex: Int,
)

/**
 * Kemampuan receiver yang diiklankan saat hello. Sender lama mengabaikan
 * field asing (serde default), jadi ini aman ditambahkan tanpa bump proto.
 */
internal data class ReceiverCapsOut(
    @SerializedName("audio") val audio: Boolean = true,
)

/**
 * Konfigurasi audio sesi dari sender (relay `sessionConfig`).
 *
 * `enabled` = audio media aktif (route TV/Both). Receiver TIDAK boleh
 * mengambil audio focus bila `enabled` false — m=audio mungkin tetap
 * dinegosiasikan (senyap) agar live switching tidak perlu renegosiasi.
 */
data class AudioSessionConfigDto(
    @SerializedName("enabled") val enabled: Boolean,
    @SerializedName("route") val route: String,
    @SerializedName("channels") val channels: Int,
    @SerializedName("sampleRate") val sampleRate: Int,
    @SerializedName("codec") val codec: String,
)

// ---- Pesan keluar (receiver → server) ----

internal data class HelloOut(
    @SerializedName("type") val type: String = Protocol.TYPE_HELLO,
    @SerializedName("role") val role: String = Protocol.ROLE_RECEIVER,
    @SerializedName("proto") val proto: Int = Protocol.VERSION,
    @SerializedName("token") val token: String,
    @SerializedName("deviceId") val deviceId: String?,
    @SerializedName("caps") val caps: ReceiverCapsOut = ReceiverCapsOut(),
)

internal data class AnswerOut(
    @SerializedName("type") val type: String = Protocol.TYPE_ANSWER,
    @SerializedName("sdp") val sdp: String,
)

internal data class IceOut(
    @SerializedName("type") val type: String = Protocol.TYPE_ICE,
    @SerializedName("candidate") val candidate: IceCandidateDto,
)

internal data class ByeOut(
    @SerializedName("type") val type: String = Protocol.TYPE_BYE,
    @SerializedName("reason") val reason: String?,
)

// ---- Pesan masuk (server → receiver) ----

/** Pesan server yang relevan untuk receiver. */
sealed interface ServerMsg {
    data class HelloOk(val proto: Int, val server: String) : ServerMsg
    data class Offer(val sdp: String) : ServerMsg
    data class Ice(val candidate: IceCandidateDto) : ServerMsg
    data class SessionConfig(val audio: AudioSessionConfigDto) : ServerMsg
    data class ErrorMsg(val code: String, val message: String) : ServerMsg
    data class Bye(val reason: String) : ServerMsg
    /** Tipe tak dikenal (mis. `receiverJoined` yang hanya untuk sender). */
    data class Unknown(val type: String) : ServerMsg
}

/**
 * Parser pesan server.
 *
 * Parsing manual berbasis `type` (bukan Gson polymorphism) agar kontrol
 * casing eksplisit dan tahan terhadap field tambahan di masa depan.
 * Mengembalikan null bila JSON tidak valid.
 */
object ServerMsgParser {
    /**
     * `disableHtmlEscaping()`: default Gson meng-escape `<>&='` menjadi
     * `\uXXXX`, sehingga SDP (`m=video`, `a=fmtp`, …) keluar sebagai
     * `\u003d` — bukan format terdokumentasi di SIGNALING_PROTOCOL.md dan
     * membengkakkan payload. Dengan dimatikan, output sama persis dengan
     * serde_json di sisi Rust.
     */
    private val gson = GsonBuilder().disableHtmlEscaping().create()

    fun parse(raw: String): ServerMsg? {
        val obj: JsonObject = runCatching { JsonParser.parseString(raw).asJsonObject }.getOrNull()
            ?: return null
        val type = obj.get("type")?.takeIf { it.isJsonPrimitive }?.asString ?: return null
        return runCatching {
            when (type) {
                Protocol.TYPE_HELLO_OK -> ServerMsg.HelloOk(
                    proto = obj.get("proto")?.asInt ?: Protocol.VERSION,
                    server = obj.get("server")?.asString ?: "",
                )
                Protocol.TYPE_OFFER -> ServerMsg.Offer(obj.get("sdp")?.asString ?: "")
                Protocol.TYPE_ICE -> {
                    val c = obj.getAsJsonObject("candidate")
                    ServerMsg.Ice(
                        IceCandidateDto(
                            candidate = c.get("candidate")?.asString ?: "",
                            sdpMid = c.get("sdpMid")?.asString ?: "0",
                            sdpMLineIndex = c.get("sdpMLineIndex")?.asInt ?: 0,
                        ),
                    )
                }
                Protocol.TYPE_ERROR -> ServerMsg.ErrorMsg(
                    code = obj.get("code")?.asString ?: "unknown",
                    message = obj.get("message")?.asString ?: "",
                )
                Protocol.TYPE_SESSION_CONFIG -> {
                    val a = obj.getAsJsonObject("audio")
                    ServerMsg.SessionConfig(
                        AudioSessionConfigDto(
                            enabled = a?.get("enabled")?.asBoolean ?: false,
                            route = a?.get("route")?.asString ?: "laptop",
                            channels = a?.get("channels")?.asInt ?: 2,
                            sampleRate = a?.get("sampleRate")?.asInt ?: 48000,
                            codec = a?.get("codec")?.asString ?: "opus",
                        ),
                    )
                }
                Protocol.TYPE_BYE -> ServerMsg.Bye(obj.get("reason")?.asString ?: "")
                else -> ServerMsg.Unknown(type)
            }
        }.getOrNull()
    }

    /** Serialisasi pesan keluar (casing via @SerializedName). */
    fun encode(msg: Any): String = gson.toJson(msg)
}
