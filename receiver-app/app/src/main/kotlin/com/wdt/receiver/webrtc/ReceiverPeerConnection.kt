package com.wdt.receiver.webrtc

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import com.wdt.receiver.signaling.IceCandidateDto
import org.webrtc.AudioTrack
import org.webrtc.DefaultVideoDecoderFactory
import org.webrtc.EglBase
import org.webrtc.IceCandidate
import org.webrtc.Logging
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.RTCStatsCollectorCallback
import org.webrtc.RTCStatsReport
import org.webrtc.RtpReceiver
import org.webrtc.RtpTransceiver
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.SurfaceViewRenderer
import org.webrtc.VideoFrame
import org.webrtc.VideoSink
import org.webrtc.VideoTrack

/**
 * Stats inbound video (untuk ukur latency/kelancaran E2E).
 *
 * @property decodedFps fps yang benar-benar didecode TV.
 * @property framesDecoded total frame terdecode.
 * @property packetsLost total paket hilang.
 * @property bitrateKbps bitrate terima (dihitung dari delta byte).
 * @property jitterBufferMs rata-rata delay jitter buffer.
 * @property rttMs round-trip time pasangan kandidat terpilih.
 */
data class ReceiverStats(
    val decodedFps: Double,
    val framesDecoded: Long,
    val packetsLost: Long,
    val bitrateKbps: Double,
    val jitterBufferMs: Double,
    val rttMs: Double,
    val audioKbps: Double = 0.0,
    val audioPacketsLost: Long = 0,
)

/** Status koneksi peer untuk UI. */
enum class PeerState {
    IDLE,
    CONNECTING,
    CONNECTED,
    DISCONNECTED,
    FAILED,
    CLOSED,
}

/**
 * PeerConnection sisi receiver.
 *
 * - STUN sama dengan sender (T3): `stun.l.google.com:19302`.
 * - Unified Plan (sender offerer, receiver answerer).
 * - Recv-only: hanya menerima video track dan merendernya ke
 *   [SurfaceViewRenderer].
 *
 * THREADING (penting, hasil perbaikan bug crash di device):
 * WebRTC memanggil `SdpObserver`/`PeerConnection.Observer` pada thread
 * internalnya (bukan main). Memanggil balik method PeerConnection atau
 * menyentuh UI dari thread itu menyebabkan `jvm.cc` abort (`Check
 * failed: false`) dan/atau `CalledFromWrongThreadException`. Karena itu
 * **semua** operasi PeerConnection dan **semua** callback keluar
 * dijalankan lewat main thread ([Handler] main looper).
 */
class ReceiverPeerConnection(
    context: Context,
    private val onIceCandidate: (IceCandidateDto) -> Unit,
    private val onStateChange: (PeerState) -> Unit,
    private val onError: (String) -> Unit,
    private val onFirstFrame: () -> Unit = {},
) {
    private val appContext = context.applicationContext
    private val main = Handler(Looper.getMainLooper())

    val eglBase: EglBase = EglBase.create()
    private val factory: PeerConnectionFactory
    private var peerConnection: PeerConnection? = null
    private var renderer: SurfaceViewRenderer? = null

    private val attachedTracks = HashSet<String>()
    private var firstFrameNotified = false

    /**
     * Audio track masuk (negosiasi pre-negotiated) + status aktif yang
     * diminta SessionConfig. Track hanya di-enable saat `audioEnabledRequested`
     * true (route TV/Both); audio focus tidak diambil saat false.
     */
    private var audioTrack: AudioTrack? = null
    private var audioEnabledRequested = false
    private val audioFocus = AudioFocusController(AndroidAudioFocusSink(appContext))

    /**
     * Kandidat remote yang tiba sebelum local description selesai diterapkan.
     * libwebrtc membuang kandidat bila JsepTransport belum ada:
     * "Not adding candidate because the JsepTransport doesn't exist."
     * Jadi kita buffer dulu, lalu flush setelah setLocalDescription sukses.
     */
    private val pendingRemoteCandidates = mutableListOf<IceCandidateDto>()
    private var localDescriptionApplied = false

    /** Loop polling stats (`getStats`) sampai di-stop. */
    private var statsRunning = false
    private var lastBytesReceived: Long = -1
    private var lastAudioBytes: Long = -1
    private var lastStatsAtMs: Long = 0

    init {
        ensureFactoryInitialized(appContext)
        factory = PeerConnectionFactory.builder()
            .setVideoDecoderFactory(DefaultVideoDecoderFactory(eglBase.eglBaseContext))
            .createPeerConnectionFactory()
    }

    private fun runOnMain(block: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) block() else main.post(block)
    }

    /** Attach renderer tujuan (fullscreen). */
    fun attachRenderer(renderer: SurfaceViewRenderer) {
        runOnMain {
            renderer.init(eglBase.eglBaseContext, null)
            renderer.setEnableHardwareScaler(true)
            renderer.setMirror(false)
            this.renderer = renderer
        }
    }

    /** Buat PeerConnection (idempoten). */
    fun start() {
        runOnMain {
            if (peerConnection != null) return@runOnMain
            val iceServers = listOf(
                PeerConnection.IceServer.builder(STUN_URL).createIceServer(),
            )
            val config = PeerConnection.RTCConfiguration(iceServers).apply {
                sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
                bundlePolicy = PeerConnection.BundlePolicy.MAXBUNDLE
            }
            val pc = factory.createPeerConnection(config, Observer())
            if (pc == null) {
                emitError("gagal membuat PeerConnection")
                return@runOnMain
            }
            peerConnection = pc
            localDescriptionApplied = false
            pendingRemoteCandidates.clear()
            emitState(PeerState.CONNECTING)
        }
    }

    /**
     * Set remote offer lalu hasilkan answer.
     *
     * Seluruh rantai (setRemoteDescription → createAnswer →
     * setLocalDescription) dijalankan di main thread, satu langkah per
     * callback — bukan re-entrant di dalam callback WebRTC.
     */
    fun setRemoteOffer(
        sdp: String,
        onAnswerReady: (String) -> Unit,
        onFailure: (String) -> Unit,
    ) {
        runOnMain {
            val pc = peerConnection ?: run {
                onFailure("PeerConnection belum siap")
                return@runOnMain
            }
            val offer = SessionDescription(SessionDescription.Type.OFFER, sdp)
            pc.setRemoteDescription(object : SdpObserver {
                override fun onCreateSuccess(desc: SessionDescription?) = Unit
                override fun onSetSuccess() {
                    runOnMain { doCreateAnswer(pc, onAnswerReady, onFailure) }
                }

                override fun onCreateFailure(error: String?) =
                    fail(onFailure, "setRemoteDescription gagal: ${error ?: "unknown"}")

                override fun onSetFailure(error: String?) =
                    fail(onFailure, "setRemoteDescription gagal: ${error ?: "unknown"}")
            }, offer)
        }
    }

    private fun doCreateAnswer(
        pc: PeerConnection,
        onAnswerReady: (String) -> Unit,
        onFailure: (String) -> Unit,
    ) {
        runOnMain {
            pc.createAnswer(object : SdpObserver {
                override fun onCreateSuccess(desc: SessionDescription?) {
                    if (desc == null) {
                        fail(onFailure, "createAnswer menghasilkan null")
                        return
                    }
                    runOnMain { doSetLocalDescription(pc, desc, onAnswerReady, onFailure) }
                }

                override fun onSetSuccess() = Unit
                override fun onCreateFailure(error: String?) =
                    fail(onFailure, "createAnswer gagal: ${error ?: "unknown"}")

                override fun onSetFailure(error: String?) = Unit
            }, MediaConstraints())
        }
    }

    private fun doSetLocalDescription(
        pc: PeerConnection,
        desc: SessionDescription,
        onAnswerReady: (String) -> Unit,
        onFailure: (String) -> Unit,
    ) {
        runOnMain {
            pc.setLocalDescription(object : SdpObserver {
                override fun onCreateSuccess(d: SessionDescription?) = Unit
                override fun onSetSuccess() {
                    flushPendingCandidates(pc)
                    onAnswerReady(desc.description)
                }
                override fun onCreateFailure(error: String?) = Unit
                override fun onSetFailure(error: String?) =
                    fail(onFailure, "setLocalDescription gagal: ${error ?: "unknown"}")
            }, desc)
        }
    }

    /** Tambah ICE candidate dari sender (trickle). */
    fun addIceCandidate(dto: IceCandidateDto) {
        runOnMain {
            val pc = peerConnection ?: return@runOnMain
            if (!localDescriptionApplied) {
                // Transport belum siap; simpan agar tidak dibuang libwebrtc.
                pendingRemoteCandidates.add(dto)
                Log.i(TAG, "ICE remote di-buffer (transport belum siap): ${dto.candidate}")
                return@runOnMain
            }
            addIceNow(pc, dto)
        }
    }

    private fun addIceNow(pc: PeerConnection, dto: IceCandidateDto) {
        // Harden: mid kosong membuat libwebrtc membuang kandidat
        // ("JsepTransport doesn't exist"). Offer kita punya m-line video
        // di index 0 → default "0".
        val mid = dto.sdpMid.ifBlank { "0" }
        runCatching {
            pc.addIceCandidate(IceCandidate(mid, dto.sdpMLineIndex, dto.candidate))
        }.onFailure { Log.w(TAG, "addIceCandidate gagal", it) }
    }

    /** Dipanggil setelah setLocalDescription sukses. */
    private fun flushPendingCandidates(pc: PeerConnection) {
        runOnMain {
            localDescriptionApplied = true
            if (pendingRemoteCandidates.isEmpty()) return@runOnMain
            Log.i(TAG, "flush ${pendingRemoteCandidates.size} kandidat remote tertunda")
            val buffered = pendingRemoteCandidates.toList()
            pendingRemoteCandidates.clear()
            for (dto in buffered) addIceNow(pc, dto)
        }
    }

    /**
     * Mulai polling `getStats` tiap [intervalMs] dan laporkan ke [onStats]
     * (dipanggil di main thread).
     */
    fun startStatsLoop(intervalMs: Long = 2000, onStats: (ReceiverStats) -> Unit) {
        if (statsRunning) return
        statsRunning = true
        lastBytesReceived = -1
        lastStatsAtMs = 0

        val tick = object : Runnable {
            override fun run() {
                if (!statsRunning) return
                val self = this
                runOnMain {
                    peerConnection?.getStats(object : RTCStatsCollectorCallback {
                        override fun onStatsDelivered(report: RTCStatsReport) {
                            val parsed = runCatching { parseStats(report) }.getOrNull()
                            if (parsed != null) runOnMain { onStats(parsed) }
                            if (statsRunning) main.postDelayed(self, intervalMs)
                        }
                    })
                }
            }
        }
        main.postDelayed(tick, intervalMs)
    }

    /** Hentikan polling stats. */
    fun stopStatsLoop() {
        statsRunning = false
    }

    /** Ekstrak stats inbound-rtp video + RTT candidate-pair. */
    private fun parseStats(report: RTCStatsReport): ReceiverStats {
        var fps = 0.0
        var frames = 0L
        var lost = 0L
        var bytes = 0L
        var jbDelay = 0.0
        var jbCount = 0.0
        var rtt = 0.0
        var audioBytes = 0L
        var audioLost = 0L

        for (st in report.statsMap.values) {
            when (st.type) {
                "inbound-rtp" -> {
                    val kind = (st.members["kind"] as? String) ?: ""
                    val isAudio = kind == "audio" ||
                        (kind.isEmpty() && st.members.containsKey("totalAudioEnergy"))
                    if (isAudio) {
                        audioBytes = memberLong(st.members, "bytesReceived") ?: audioBytes
                        audioLost = memberLong(st.members, "packetsLost") ?: audioLost
                        continue
                    }
                    val isVideo = kind == "video" ||
                        st.members.containsKey("framesDecoded")
                    if (!isVideo) continue
                    fps = memberDouble(st.members, "framesPerSecond") ?: fps
                    frames = memberLong(st.members, "framesDecoded") ?: frames
                    lost = memberLong(st.members, "packetsLost") ?: lost
                    bytes = memberLong(st.members, "bytesReceived") ?: bytes
                    jbDelay = memberDouble(st.members, "jitterBufferDelay") ?: jbDelay
                    jbCount =
                        memberDouble(st.members, "jitterBufferEmittedCount") ?: jbCount
                }
                "candidate-pair" -> {
                    val state = (st.members["state"] as? String) ?: ""
                    if (state == "succeeded") {
                        rtt = memberDouble(st.members, "currentRoundTripTime") ?: rtt
                    }
                }
            }
        }

        // Bitrate dari delta byte antar polling.
        val nowMs = System.currentTimeMillis()
        var kbps = 0.0
        if (lastBytesReceived >= 0 && lastStatsAtMs > 0 && nowMs > lastStatsAtMs) {
            val deltaBytes = (bytes - lastBytesReceived).coerceAtLeast(0)
            kbps = deltaBytes * 8.0 / (nowMs - lastStatsAtMs)
        }
        var audioKbps = 0.0
        if (lastAudioBytes >= 0 && lastStatsAtMs > 0 && nowMs > lastStatsAtMs) {
            val deltaBytes = (audioBytes - lastAudioBytes).coerceAtLeast(0)
            audioKbps = deltaBytes * 8.0 / (nowMs - lastStatsAtMs)
        }
        lastBytesReceived = bytes
        lastAudioBytes = audioBytes
        lastStatsAtMs = nowMs

        val jbMs = if (jbCount > 0) jbDelay / jbCount * 1000.0 else 0.0
        return ReceiverStats(
            decodedFps = fps,
            framesDecoded = frames,
            packetsLost = lost,
            bitrateKbps = kbps,
            jitterBufferMs = jbMs,
            rttMs = rtt * 1000.0,
            audioKbps = audioKbps,
            audioPacketsLost = audioLost,
        )
    }

    private fun memberLong(members: Map<String, Any>, key: String): Long? =
        when (val v = members[key]) {
            is Number -> v.toLong()
            is String -> v.toLongOrNull()
            else -> null
        }

    private fun memberDouble(members: Map<String, Any>, key: String): Double? =
        when (val v = members[key]) {
            is Number -> v.toDouble()
            is String -> v.toDoubleOrNull()
            else -> null
        }

    /** Tutup peer connection + renderer. */
    fun close() {
        stopStatsLoop()
        runOnMain {
            // Lepaskan audio focus + matikan audio SEBELUM menutup PC, agar
            // tidak ada suara/focus tersisa setelah sesi berhenti.
            audioTrack?.setEnabled(false)
            audioFocus.release()
            audioTrack = null
            audioEnabledRequested = false
            renderer?.let { r ->
                runCatching {
                    r.clearImage()
                    r.release()
                }
            }
            renderer = null
            peerConnection?.let { pc -> runCatching { pc.close() } }
            peerConnection = null
            attachedTracks.clear()
            firstFrameNotified = false
            emitState(PeerState.CLOSED)
        }
    }

    /**
     * Aktifkan/nonaktifkan audio sesuai SessionConfig.
     *
     * Saat `enabled=false` (route Laptop/Muted) track tetap terpasang tetapi
     * dinonaktifkan dan audio focus TIDAK diambil — receiver tidak boleh
     * menguasai audio TV bila sesi menyatakan audio tidak aktif.
     */
    fun setAudioEnabled(enabled: Boolean) {
        runOnMain {
            audioEnabledRequested = enabled
            audioTrack?.setEnabled(enabled)
            audioFocus.setEnabled(enabled && audioTrack != null)
            Log.i(TAG, "audio route: enabled=$enabled track=${audioTrack?.id()}")
        }
    }

    /** Apakah audio sedang aktif (untuk UI). */
    fun isAudioEnabled(): Boolean = audioEnabledRequested && audioTrack != null

    private fun attachAudioTrack(track: AudioTrack) {
        if (audioTrack?.id() == track.id()) return
        audioTrack = track
        track.setEnabled(audioEnabledRequested)
        audioFocus.setEnabled(audioEnabledRequested)
        Log.i(TAG, "audio track ter-attach: ${track.id()} enabled=$audioEnabledRequested")
    }

    private fun attachVideoTrack(track: VideoTrack) {
        val id = track.id()
        if (attachedTracks.contains(id)) return
        val r = renderer
        if (r == null) {
            Log.w(TAG, "video track tiba sebelum renderer siap")
            return
        }
        attachedTracks.add(id)
        // Probe latensi (debug T6) opt-in; default renderer langsung.
        if (ATTACH_LATENCY_PROBE) {
            track.addSink(LatencyProbeSink(r))
        } else {
            track.addSink(r)
        }
        track.addSink(object : VideoSink {
            override fun onFrame(frame: VideoFrame) {
                if (!firstFrameNotified) {
                    firstFrameNotified = true
                    runOnMain { onFirstFrame() }
                }
            }
        })
        Log.i(TAG, "video track ter-attach: $id")
    }

    private fun emitState(state: PeerState) = runOnMain { onStateChange(state) }

    private fun emitError(message: String) = runOnMain { onError(message) }

    private fun fail(onFailure: (String) -> Unit, message: String) =
        runOnMain { onFailure(message) }

    private inner class Observer : PeerConnection.Observer {
        override fun onIceCandidate(candidate: IceCandidate?) {
            candidate ?: return
            // Kirim ke signaling (OkHttp aman dari thread mana pun), tapi
            // dispatch lewat main agar akses field seragam & tanpa race.
            val dto = IceCandidateDto(
                candidate = candidate.sdp,
                sdpMid = candidate.sdpMid ?: "0",
                sdpMLineIndex = candidate.sdpMLineIndex,
            )
            runOnMain { onIceCandidate(dto) }
        }

        override fun onTrack(transceiver: RtpTransceiver?) {
            val track = transceiver?.receiver?.track()
            when (track) {
                is VideoTrack -> runOnMain { attachVideoTrack(track) }
                is AudioTrack -> runOnMain { attachAudioTrack(track) }
            }
        }

        @Deprecated("Compat Plan B; Unified Plan memakai onTrack(transceiver).")
        override fun onAddTrack(receiver: RtpReceiver?, mediaStreams: Array<out MediaStream>?) {
            val track = receiver?.track()
            when (track) {
                is VideoTrack -> runOnMain { attachVideoTrack(track) }
                is AudioTrack -> runOnMain { attachAudioTrack(track) }
            }
        }

        override fun onConnectionChange(newState: PeerConnection.PeerConnectionState?) {
            val mapped = when (newState) {
                PeerConnection.PeerConnectionState.NEW -> PeerState.IDLE
                PeerConnection.PeerConnectionState.CONNECTING -> PeerState.CONNECTING
                PeerConnection.PeerConnectionState.CONNECTED -> PeerState.CONNECTED
                PeerConnection.PeerConnectionState.DISCONNECTED -> PeerState.DISCONNECTED
                PeerConnection.PeerConnectionState.FAILED -> PeerState.FAILED
                PeerConnection.PeerConnectionState.CLOSED -> PeerState.CLOSED
                else -> PeerState.IDLE
            }
            if (mapped == PeerState.FAILED) emitError("koneksi WebRTC gagal (ICE/DTLS)")
            emitState(mapped)
        }

        override fun onIceConnectionChange(newState: PeerConnection.IceConnectionState?) {
            Log.i(TAG, "iceConnectionState=$newState")
        }

        override fun onIceConnectionReceivingChange(receiving: Boolean) = Unit

        override fun onIceGatheringChange(newState: PeerConnection.IceGatheringState?) {
            Log.i(TAG, "iceGatheringState=$newState")
        }

        override fun onIceCandidateError(event: org.webrtc.IceCandidateErrorEvent?) {
            Log.w(TAG, "iceCandidateError: ${event?.errorCode} ${event?.errorText} url=${event?.url}")
        }

        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>?) = Unit
        override fun onSignalingChange(newState: PeerConnection.SignalingState?) {
            Log.i(TAG, "signalingState=$newState")
        }
        override fun onAddStream(stream: MediaStream?) = Unit
        override fun onRemoveStream(stream: MediaStream?) = Unit
        override fun onDataChannel(channel: org.webrtc.DataChannel?) = Unit
        override fun onRenegotiationNeeded() = Unit
    }

    companion object {
        private const val TAG = "ReceiverPeerConnection"
        private const val STUN_URL = "stun:stun.l.google.com:19302"

        /**
         * Debug T6: pasang [LatencyProbeSink] untuk mengukur usia frame di sink.
         * Default `false` (produksi) — probe mengonversi tiap frame ke I420
         * (beban CPU), hanya untuk diagnosis latency.
         */
        private const val ATTACH_LATENCY_PROBE = false

        /**
         * Log internal libwebrtc. Dibiarkan false untuk production; ubah ke
         * true saat debug interop/ICE di device (log sangat verbose).
         */
        private const val WEBRTC_DEBUG_LOG = false

        @Volatile
        private var factoryInitialized = false

        private fun ensureFactoryInitialized(context: Context) {
            if (factoryInitialized) return
            synchronized(ReceiverPeerConnection::class.java) {
                if (factoryInitialized) return
                PeerConnectionFactory.initialize(
                    PeerConnectionFactory.InitializationOptions.builder(context)
                        .setEnableInternalTracer(false)
                        .createInitializationOptions(),
                )
                if (WEBRTC_DEBUG_LOG) {
                    // HARUS setelah initialize(): native lib dimuat di sana,
                    // memanggil sebelumnya -> UnsatisfiedLinkError.
                    Logging.enableLogToDebugOutput(Logging.Severity.LS_VERBOSE)
                }
                factoryInitialized = true
            }
        }
    }
}
