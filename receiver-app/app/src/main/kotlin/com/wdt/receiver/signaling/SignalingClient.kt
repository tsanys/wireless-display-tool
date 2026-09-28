package com.wdt.receiver.signaling

import android.os.Handler
import android.os.Looper
import android.util.Log
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.TimeUnit

/**
 * Client WebSocket signaling (receiver).
 *
 * Mengikuti `docs/SIGNALING_PROTOCOL.md` persis: hello{role,proto,token,deviceId},
 * terima helloOk/offer/ice/error/bye, kirim answer/ice/bye.
 *
 * Callback diteruskan ke main thread ([Handler] main looper) agar aman
 * dipakai UI.
 */
class SignalingClient(
    private val onOpen: () -> Unit,
    private val onHelloOk: (server: String) -> Unit,
    private val onOffer: (sdp: String) -> Unit,
    private val onIce: (IceCandidateDto) -> Unit,
    private val onSessionConfig: (AudioSessionConfigDto) -> Unit = {},
    private val onError: (code: String, message: String) -> Unit,
    private val onClosed: (reason: String) -> Unit,
) {
    private val main = Handler(Looper.getMainLooper())
    private val client: OkHttpClient = OkHttpClient.Builder()
        .pingInterval(20, TimeUnit.SECONDS)
        .connectTimeout(10, TimeUnit.SECONDS)
        .build()

    private var webSocket: WebSocket? = null
    private var helloSent = false
    private var closedByUs = false

    /**
     * Buka koneksi ke signaling server sender.
     *
     * @param host alamat IP sender.
     * @param port port signaling (default [Protocol.DEFAULT_PORT]).
     * @param token token pairing 6-digit.
     * @param deviceId label TV (opsional).
     */
    fun connect(host: String, port: Int, token: String, deviceId: String?, deviceName: String?) {
        val url = "ws://$host:$port${Protocol.WS_PATH}"
        Log.i(TAG, "connect $url")
        closedByUs = false
        helloSent = false
        val request = Request.Builder().url(url).build()
        webSocket = client.newWebSocket(request, Listener(token, deviceId, deviceName))
    }

    /** Kirim SDP answer (receiver → sender via relay server). */
    fun sendAnswer(sdp: String) {
        send(ServerMsgParser.encode(AnswerOut(sdp = sdp)))
    }

    /** Kirim ICE candidate lokal (trickle). */
    fun sendIce(candidate: IceCandidateDto) {
        send(ServerMsgParser.encode(IceOut(candidate = candidate)))
    }

    /** Tutup sesi dengan sopan (kirim bye). */
    fun close(reason: String? = null) {
        closedByUs = true
        runCatching {
            send(ServerMsgParser.encode(ByeOut(reason = reason)))
            webSocket?.close(1000, reason)
        }
        webSocket = null
    }

    /** Tutup tanpa handshake (dipakai teardown Activity). */
    fun shutdown() {
        closedByUs = true
        webSocket?.cancel()
        webSocket = null
    }

    private fun send(json: String): Boolean {
        val ws = webSocket ?: return false
        val ok = ws.send(json)
        if (!ok) Log.w(TAG, "kirim pesan gagal: $json")
        return ok
    }

    private inner class Listener(
        private val token: String,
        private val deviceId: String?,
        private val deviceName: String?,
    ) : WebSocketListener() {

        override fun onOpen(webSocket: WebSocket, response: Response) {
            Log.i(TAG, "WS open")
            if (!helloSent) {
                helloSent = true
                val hello = ServerMsgParser.encode(
                    HelloOut(token = token, deviceId = deviceId, deviceName = deviceName),
                )
                webSocket.send(hello)
            }
            main.post { onOpen() }
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            val msg = ServerMsgParser.parse(text)
            if (msg == null) {
                Log.w(TAG, "pesan tak dikenal: $text")
                return
            }
            main.post {
                when (msg) {
                    is ServerMsg.HelloOk -> onHelloOk(msg.server)
                    is ServerMsg.Offer -> onOffer(msg.sdp)
                    is ServerMsg.Ice -> onIce(msg.candidate)
                    is ServerMsg.SessionConfig -> onSessionConfig(msg.audio)
                    is ServerMsg.ErrorMsg -> onError(msg.code, msg.message)
                    is ServerMsg.Bye -> onClosed(msg.reason)
                    is ServerMsg.Unknown -> Log.d(TAG, "abaikan tipe: ${msg.type}")
                }
            }
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            Log.i(TAG, "WS closing: $code $reason")
            webSocket.close(1000, null)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            Log.i(TAG, "WS closed: $code $reason")
            if (!closedByUs) main.post { onClosed(reason.ifBlank { "serverClosed" }) }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            Log.e(TAG, "WS failure", t)
            if (!closedByUs) {
                main.post { onError("networkError", t.message ?: "koneksi gagal") }
            }
        }
    }

    companion object {
        private const val TAG = "SignalingClient"
    }
}
