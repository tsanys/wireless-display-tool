package com.wdt.receiver

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import com.wdt.receiver.device.DeviceCapability
import com.wdt.receiver.discovery.NsdDiscovery
import com.wdt.receiver.discovery.SenderInfo
import com.wdt.receiver.pairing.ManualPairing
import com.wdt.receiver.pairing.PairingResult
import com.wdt.receiver.signaling.IceCandidateDto
import com.wdt.receiver.signaling.SignalingClient
import com.wdt.receiver.webrtc.PeerState
import org.webrtc.RendererCommon
import com.wdt.receiver.webrtc.ReceiverPeerConnection
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * State UI receiver.
 */
private enum class UiState {
    SCANNING,
    CONNECTING,
    CONNECTED,
    ERROR,
}

/**
 * Activity utama receiver Android TV.
 *
 * Alur: scanning (NSD + fallback manual) → connect → connecting →
 * connected (video fullscreen) → error/disconnected.
 *
 * Semua callback WebRTC/NSD/OkHttp sudah di-dispatch ke main thread oleh
 * masing-masing komponen, jadi UI update di sini aman.
 */
class MainActivity : Activity() {

    private companion object {
        const val TAG = "WdtMain"
        const val SESSION_PANEL_TIMEOUT_MS = 4_000L
        const val LOW_LATENCY_PREF = "low_latency"
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val uiHandler = android.os.Handler(android.os.Looper.getMainLooper())

    private lateinit var statusText: TextView
    private lateinit var setupStatusText: TextView
    private lateinit var setupHeadline: TextView
    private lateinit var setupDescription: TextView
    private lateinit var sessionBadge: TextView
    private lateinit var tierText: TextView
    private lateinit var senderText: TextView
    private lateinit var audioText: TextView
    private lateinit var statsText: TextView
    private lateinit var discoveryLabel: TextView
    private lateinit var senderList: LinearLayout
    private lateinit var tokenInput: EditText
    private lateinit var manualInput: EditText
    private lateinit var btnConnectManual: Button
    private lateinit var btnRetry: Button
    private lateinit var btnManualToggle: Button
    private lateinit var chkLowLatency: CheckBox
    private lateinit var btnDisconnect: Button
    private lateinit var btnHideSession: Button
    private lateinit var manualSection: View
    private lateinit var panelSetup: View
    private lateinit var sessionPanel: View
    private lateinit var videoRenderer: org.webrtc.SurfaceViewRenderer

    private lateinit var capability: DeviceCapability
    private var discovery: NsdDiscovery? = null

    private var signaling: SignalingClient? = null
    private var peer: ReceiverPeerConnection? = null

    private var state: UiState = UiState.SCANNING
    private var connectedSenderLabel: String? = null
    private var videoShown = false
    /** Status audio sesi (dari SessionConfig) untuk panel sesi. */
    private var audioActive = false
    private var audioRouteLabel = ""

    private val hideSessionPanel = object : Runnable {
        override fun run() {
            hidePlaybackPanel()
        }
    }

    /** Guard: offer hanya diproses sekali per sesi. */
    private var offerHandled = false

    /**
     * Epoch peer: callback dari peer yang sudah diganti (re-arm/offer baru)
     * harus DIABAIKAN; peer lama yang CLOSED/FAILED bisa memicu teardown
     * dan menutup signaling yang masih kita butuhkan.
     */
    private var peerEpoch = 0

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setSoftInputMode(
            WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN or
                WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING,
        )
        setContentView(R.layout.activity_main)

        statusText = findViewById(R.id.status_text)
        setupStatusText = findViewById(R.id.setup_status_text)
        setupHeadline = findViewById(R.id.setup_headline)
        setupDescription = findViewById(R.id.setup_description)
        sessionBadge = findViewById(R.id.session_badge)
        tierText = findViewById(R.id.tier_text)
        senderText = findViewById(R.id.sender_text)
        audioText = findViewById(R.id.audio_text)
        statsText = findViewById(R.id.stats_text)
        discoveryLabel = findViewById(R.id.discovery_label)
        senderList = findViewById(R.id.sender_list)
        tokenInput = findViewById(R.id.token_input)
        manualInput = findViewById(R.id.manual_input)
        btnConnectManual = findViewById(R.id.btn_connect_manual)
        btnRetry = findViewById(R.id.btn_retry)
        btnManualToggle = findViewById(R.id.btn_manual_toggle)
        chkLowLatency = findViewById(R.id.chk_low_latency)
        btnDisconnect = findViewById(R.id.btn_disconnect)
        btnHideSession = findViewById(R.id.btn_hide_session)
        manualSection = findViewById(R.id.manual_section)
        panelSetup = findViewById(R.id.panel_setup)
        sessionPanel = findViewById(R.id.session_panel)
        videoRenderer = findViewById(R.id.video_renderer)

        findViewById<TextView>(R.id.title_text).text = getString(R.string.app_title)
        findViewById<TextView>(R.id.manual_label).text = getString(R.string.manual_label)
        btnConnectManual.text = getString(R.string.btn_connect)
        btnRetry.text = getString(R.string.btn_retry)
        chkLowLatency.text = getString(R.string.low_latency_label)
        audioRouteLabel = getString(R.string.audio_route_laptop)

        // Trial WebRTC hanya dapat diterapkan sebelum factory pertama dibuat.
        val prefs = getPreferences(Context.MODE_PRIVATE)
        chkLowLatency.isChecked = prefs.getBoolean(LOW_LATENCY_PREF, false)
        chkLowLatency.setOnCheckedChangeListener { _, isChecked ->
            prefs.edit().putBoolean(LOW_LATENCY_PREF, isChecked).apply()
            if (ReceiverPeerConnection.isFactoryInitialized() || peer != null || signaling != null || state != UiState.SCANNING) {
                chkLowLatency.isChecked = !isChecked
                showTransientError(getString(R.string.low_latency_restart))
            }
        }

        // --- Capability detection (tier device) ---
        scope.launch {
            capability = DeviceCapability.detect(this@MainActivity)
            tierText.text = getString(R.string.tier_fmt, capability.summary())
        }

        // --- NSD discovery ---
        discovery = NsdDiscovery(
            context = this,
            onSendersChanged = { senders -> renderSenders(senders) },
            onError = { message -> showTransientError(message) },
        )

        btnRetry.setOnClickListener { backToScanning() }
        btnConnectManual.setOnClickListener { connectManual() }
        btnManualToggle.setOnClickListener {
            val opening = manualSection.visibility != View.VISIBLE
            manualSection.visibility = if (opening) View.VISIBLE else View.GONE
            btnManualToggle.text = getString(
                if (opening) R.string.btn_manual_hide else R.string.btn_manual_show,
            )
            if (opening) manualInput.requestFocus()
        }
        btnDisconnect.setOnClickListener { confirmDisconnect() }
        btnHideSession.setOnClickListener { hidePlaybackPanel() }
        manualInput.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_DONE) {
                closeKeyboard(manualInput)
                connectManual()
                true
            } else {
                false
            }
        }
        manualInput.setOnKeyListener { _, keyCode, event ->
            if (event.action == KeyEvent.ACTION_DOWN && keyCode == KeyEvent.KEYCODE_DPAD_DOWN) {
                btnConnectManual.requestFocus()
                true
            } else {
                false
            }
        }
        tokenInput.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_DONE) {
                closeKeyboard(tokenInput)
                (senderList.getChildAt(0) ?: btnRetry).requestFocus()
                true
            } else {
                false
            }
        }
        tokenInput.showSoftInputOnFocus = false
        manualInput.showSoftInputOnFocus = false
        tokenInput.setOnClickListener { openKeyboard(tokenInput) }
        manualInput.setOnClickListener { openKeyboard(manualInput) }
        tokenInput.setOnFocusChangeListener { _, hasFocus ->
            if (!hasFocus) tokenInput.showSoftInputOnFocus = false
        }
        manualInput.setOnFocusChangeListener { _, hasFocus ->
            if (!hasFocus) manualInput.showSoftInputOnFocus = false
        }
        tokenInput.setOnKeyListener { _, keyCode, event ->
            if (event.action == KeyEvent.ACTION_DOWN && keyCode == KeyEvent.KEYCODE_DPAD_DOWN) {
                (senderList.getChildAt(0) ?: btnRetry).requestFocus()
                true
            } else {
                false
            }
        }

        setStatus(UiState.SCANNING, getString(R.string.discovery_searching))
        discoveryLabel.text = getString(R.string.discovery_searching)
        tokenInput.post {
            val keyboard = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
            keyboard.hideSoftInputFromWindow(tokenInput.windowToken, 0)
            tokenInput.requestFocus()
        }
    }

    override fun onResume() {
        super.onResume()
        if (state == UiState.SCANNING) restartDiscovery()
    }

    override fun onPause() {
        discovery?.stop()
        super.onPause()
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (
            event.action == KeyEvent.ACTION_DOWN &&
            sessionPanel.visibility == View.VISIBLE &&
            event.keyCode != KeyEvent.KEYCODE_BACK
        ) {
            uiHandler.removeCallbacks(hideSessionPanel)
            uiHandler.postDelayed(hideSessionPanel, SESSION_PANEL_TIMEOUT_MS)
        }
        if (event.action == KeyEvent.ACTION_DOWN && event.keyCode == KeyEvent.KEYCODE_DPAD_DOWN) {
            when (currentFocus) {
                tokenInput -> {
                    (senderList.getChildAt(0) ?: btnRetry).requestFocus()
                    return true
                }
                manualInput -> {
                    btnConnectManual.requestFocus()
                    return true
                }
            }
        }
        val revealKey = event.keyCode == KeyEvent.KEYCODE_DPAD_CENTER ||
            event.keyCode == KeyEvent.KEYCODE_ENTER ||
            event.keyCode == KeyEvent.KEYCODE_MENU
        if (
            event.action == KeyEvent.ACTION_DOWN &&
            revealKey &&
            panelSetup.visibility != View.VISIBLE &&
            sessionPanel.visibility != View.VISIBLE
        ) {
            showPlaybackPanel(autoHide = true)
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    @Deprecated("Android TV still routes remote Back through this callback")
    override fun onBackPressed() {
        if (sessionPanel.visibility == View.VISIBLE && panelSetup.visibility != View.VISIBLE) {
            hidePlaybackPanel()
        } else if (peer != null || signaling != null || state == UiState.CONNECTED || state == UiState.CONNECTING) {
            confirmDisconnect()
        } else {
            super.onBackPressed()
        }
    }

    override fun onDestroy() {
        uiHandler.removeCallbacks(hideSessionPanel)
        teardownSession(closeSignaling = true)
        discovery?.stop()
        discovery = null
        scope.cancel()
        super.onDestroy()
    }

    // ---------- Discovery & pairing ----------

    private fun restartDiscovery() {
        discovery?.stop()
        senderList.removeAllViews()
        discoveryLabel.text = getString(R.string.discovery_searching)
        discovery?.start()
    }

    private fun renderSenders(senders: List<SenderInfo>) {
        if (state != UiState.SCANNING) return
        senderList.removeAllViews()
        if (senders.isEmpty()) {
            discoveryLabel.text = getString(R.string.discovery_empty)
            return
        }
        discoveryLabel.text = getString(R.string.discovery_found)
        for (sender in senders) {
            val label = sender.instanceName.removePrefix("WDT ").ifBlank {
                getString(R.string.audio_route_laptop)
            }
            val btn = Button(this).apply {
                text = getString(R.string.sender_available_fmt, label)
                isAllCaps = false
                maxLines = 2
                ellipsize = android.text.TextUtils.TruncateAt.END
                gravity = Gravity.START or Gravity.CENTER_VERTICAL
                textSize = 16f
                setTextColor(resources.getColorStateList(R.color.selector_secondary_text))
                setBackgroundResource(R.drawable.selector_device_button)
                minHeight = dp(70)
                setPadding(dp(20), dp(10), dp(20), dp(10))
                layoutParams = LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT,
                    LinearLayout.LayoutParams.WRAP_CONTENT,
                ).apply { bottomMargin = dp(10) }
                setOnClickListener {
                    val token = promptToken(sender.instanceName) ?: return@setOnClickListener
                    connectTo(sender.host, sender.port, token, label)
                }
            }
            senderList.addView(btn)
        }
    }

    /**
     * Token pairing untuk sender hasil discovery.
     *
     * Sender hasil discovery hanya memerlukan token 6 digit. Alamat lengkap
     * sengaja berada di fallback terpisah agar dua tugas ini tidak tercampur.
     */
    private fun promptToken(senderName: String): String? {
        val raw = tokenInput.text?.toString()?.trim().orEmpty()
        ManualPairing.parseTokenOnly(raw)?.let { return it }
        Toast.makeText(
            this,
            getString(R.string.pairing_token_prompt_fmt, senderName),
            Toast.LENGTH_LONG,
        ).show()
        tokenInput.requestFocus()
        return null
    }

    private fun connectManual() {
        when (val result = ManualPairing.parse(manualInput.text?.toString().orEmpty())) {
            is PairingResult.Failure -> {
                discoveryLabel.text = getString(R.string.manual_invalid)
                showTransientError(result.reason)
                manualInput.requestFocus()
            }
            is PairingResult.Success -> connectTo(
                host = result.info.host,
                port = result.info.port,
                token = result.info.token,
                label = result.info.canonical(),
            )
        }
    }

    // ---------- Session ----------

    private fun connectTo(host: String, port: Int, token: String, label: String) {
        teardownSession(closeSignaling = true)
        offerHandled = false
        videoShown = false
        connectedSenderLabel = label

        senderText.text = getString(R.string.sender_fmt, label)
        setStatus(UiState.CONNECTING, getString(R.string.status_contacting_fmt, label))
        panelSetup.visibility = View.VISIBLE
        sessionPanel.visibility = View.GONE
        discovery?.stop()

        // WebRTC peer (renderer sudah siap di layout).
        peer = buildPeer()

        // Signaling client.
        signaling = SignalingClient(
            onOpen = { setStatus(UiState.CONNECTING, getString(R.string.status_signaling_open)) },
            onHelloOk = { server ->
                setStatus(UiState.CONNECTING, getString(R.string.status_pairing_ok_fmt, server))
            },
            onOffer = { sdp -> handleOffer(sdp) },
            onIce = { candidate ->
                Log.i(TAG, "ICE dari sender <- ${candidate.candidate}")
                peer?.addIceCandidate(candidate)
            },
            onSessionConfig = { audio -> onSessionConfig(audio) },
            onError = { code, message -> onSignalingError(code, message) },
            onClosed = { reason -> onRemoteClosed(reason) },
        ).also { it.connect(host, port, token, deviceLabel()) }
    }

    /// Bangun ReceiverPeerConnection baru dengan callback lengkap.
    /// Dipakai saat connect awal maupun saat re-arm (offer berikutnya
    /// setelah mirror-stop / Start ulang dari sender).
    private fun buildPeer(): ReceiverPeerConnection {
        val epoch = ++peerEpoch
        fun current() = epoch == peerEpoch
        val lowLatency = getPreferences(Context.MODE_PRIVATE).getBoolean(LOW_LATENCY_PREF, false)
        Log.i(TAG, "peer lowLatency=$lowLatency")
        val p = ReceiverPeerConnection(
            context = this,
            lowLatency = lowLatency,
            onIceCandidate = { candidate: IceCandidateDto ->
                if (current()) {
                    Log.i(TAG, "ICE lokal -> ${candidate.candidate}")
                    signaling?.sendIce(candidate)
                }
            },
            onStateChange = { peerState -> if (current()) onPeerState(peerState) },
            onError = { message -> if (current()) onSessionError(message) },
            onFirstFrame = { if (current()) onVideoFrame() },
        )
        p.attachRenderer(videoRenderer)
        // Letterbox: jangan rentangkan (stretch) konten 16:10 ke panel 16:9 —
        // memperbaiki keluhan "gambar buram/gepeng".
        videoRenderer.setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FIT)
        p.start()
        p.startStatsLoop { st ->
            Log.i(TAG, "stats: $st")
            statsText.text = getString(
                R.string.stats_fmt,
                st.decodedFps,
                st.framesDecoded,
                st.packetsLost,
                st.bitrateKbps,
                st.jitterBufferMs,
                st.rttMs,
            )
            renderAudioStatus(st.audioKbps)
        }
        return p
    }

    /** Terapkan konfigurasi audio dari sender (SessionConfig). */
    private fun onSessionConfig(audio: com.wdt.receiver.signaling.AudioSessionConfigDto) {
        Log.i(
            TAG,
            "sessionConfig: enabled=${audio.enabled} route=${audio.route} " +
                "${audio.sampleRate}Hz/${audio.channels}ch ${audio.codec}",
        )
        audioActive = audio.enabled
        audioRouteLabel = when (audio.route) {
            "tv" -> getString(R.string.audio_route_tv)
            "both" -> getString(R.string.audio_route_both)
            "muted" -> getString(R.string.audio_route_muted)
            else -> getString(R.string.audio_route_laptop)
        }
        peer?.setAudioEnabled(audio.enabled)
        renderAudioStatus(0.0)
    }

    /** Perbarui baris audio di panel sesi (lokasi + bitrate bila aktif). */
    private fun renderAudioStatus(audioKbps: Double) {
        if (!audioActive) {
            audioText.text = getString(R.string.audio_status_off_fmt, audioRouteLabel)
            return
        }
        val rate = if (audioKbps > 0) " · %.0f kbps".format(audioKbps) else ""
        audioText.text = getString(R.string.audio_status_fmt, audioRouteLabel, rate)
    }

    private fun handleOffer(sdp: String) {
        // Offer bisa datang berulang (Start ulang dari sender setelah Stop):
        // tutup peer lama, buat peer baru, proses offer di atasnya.
        peerEpoch++ // invalidate callback peer lama sebelum dibuang
        peer?.let { old ->
            old.stopStatsLoop()
            old.close()
        }
        val p = buildPeer()
        peer = p
        offerHandled = true
        setStatus(UiState.CONNECTING, getString(R.string.status_offer_received))
        p.setRemoteOffer(
            sdp = sdp,
            onAnswerReady = { answer ->
                signaling?.sendAnswer(answer)
                setStatus(UiState.CONNECTING, getString(R.string.status_answer_sent))
            },
            onFailure = { error -> onSessionError(error) },
        )
    }

    private fun onPeerState(peerState: PeerState) {
        Log.i(TAG, "peerState=$peerState")
        when (peerState) {
            PeerState.CONNECTED -> {
                setStatus(UiState.CONNECTED, getString(R.string.status_connected_waiting_video))
            }
            PeerState.CONNECTING -> setStatus(UiState.CONNECTING, getString(R.string.status_connecting_peer))
            PeerState.DISCONNECTED -> setStatus(
                UiState.CONNECTING,
                getString(R.string.status_peer_recovering),
            )
            PeerState.FAILED -> onSessionError(getString(R.string.err_webrtc_failed))
            PeerState.CLOSED -> Unit
            PeerState.IDLE -> Unit
        }
    }

    private fun onVideoFrame() {
        if (videoShown) return
        videoShown = true
        setStatus(UiState.CONNECTED, getString(R.string.status_video_shown))
        videoRenderer.visibility = View.VISIBLE
        panelSetup.visibility = View.GONE
        sessionPanel.clearFocus()
        showPlaybackPanel(autoHide = true)
    }

    private fun onSignalingError(code: String, message: String) {
        Log.i(TAG, "signalingError code=$code message=$message")
        val human = when (code) {
            "badToken" -> getString(R.string.err_bad_token)
            "senderBusy" -> getString(R.string.err_sender_busy)
            "protoMismatch" -> getString(R.string.err_proto_mismatch)
            "networkError" -> getString(R.string.err_network)
            else -> getString(R.string.err_generic)
        }
        onSessionError(human)
    }

    private fun onRemoteClosed(reason: String) {
        Log.i(TAG, "remoteClosed reason=$reason")
        if (state == UiState.ERROR) return
        if (reason == "mirror-stop") {
            // Sender berhenti mirror tapi sesi WS tetap hidup: buang peer,
            // re-arm untuk offer berikutnya (Start ulang tanpa Connect lagi).
            peerEpoch++ // callback peer lama diabaikan
            peer?.let { old ->
                old.stopStatsLoop()
                old.close()
            }
            peer = null
            offerHandled = false
            videoShown = false
            audioActive = false
            audioText.text = ""
            statsText.text = ""
            setStatus(UiState.CONNECTED, getString(R.string.status_mirror_stopped), paused = true)
            sessionPanel.clearFocus()
            showPlaybackPanel(autoHide = true)
            return
        }
        Toast.makeText(this, getString(R.string.session_ended_fmt, reason), Toast.LENGTH_SHORT).show()
        backToScanning()
    }

    private fun onSessionError(message: String) {
        Log.w(TAG, "sessionError: $message")
        setStatus(UiState.ERROR, message)
        Toast.makeText(this, message, Toast.LENGTH_LONG).show()
        teardownSession(closeSignaling = true)
        panelSetup.visibility = View.VISIBLE
        sessionPanel.visibility = View.GONE
        discoveryLabel.text = getString(R.string.discovery_retry_hint)
        btnRetry.requestFocus()
    }

    private fun backToScanning() {
        teardownSession(closeSignaling = true)
        connectedSenderLabel = null
        videoRenderer.visibility = View.VISIBLE
        panelSetup.visibility = View.VISIBLE
        sessionPanel.visibility = View.GONE
        setStatus(UiState.SCANNING, getString(R.string.discovery_searching))
        restartDiscovery()
    }

    private fun teardownSession(closeSignaling: Boolean) {
        Log.i(TAG, "teardownSession closeSignaling=$closeSignaling state=$state")
        peerEpoch++
        uiHandler.removeCallbacks(hideSessionPanel)
        peer?.stopStatsLoop()
        statsText.text = ""
        audioText.text = ""
        audioActive = false
        peer?.close()
        peer = null
        if (closeSignaling) signaling?.close("receiver-stop") else signaling?.shutdown()
        signaling = null
        offerHandled = false
        videoShown = false
    }

    private fun showPlaybackPanel(autoHide: Boolean) {
        uiHandler.removeCallbacks(hideSessionPanel)
        sessionPanel.visibility = View.VISIBLE
        if (autoHide) uiHandler.postDelayed(hideSessionPanel, SESSION_PANEL_TIMEOUT_MS)
    }

    private fun hidePlaybackPanel() {
        uiHandler.removeCallbacks(hideSessionPanel)
        sessionPanel.clearFocus()
        sessionPanel.visibility = View.GONE
    }

    private fun openKeyboard(input: EditText) {
        input.showSoftInputOnFocus = true
        input.requestFocus()
        uiHandler.post {
            val keyboard = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
            keyboard.showSoftInput(input, InputMethodManager.SHOW_IMPLICIT)
        }
    }

    private fun closeKeyboard(input: EditText) {
        val keyboard = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        keyboard.hideSoftInputFromWindow(input.windowToken, 0)
        input.showSoftInputOnFocus = false
    }

    private fun confirmDisconnect() {
        uiHandler.removeCallbacks(hideSessionPanel)
        val dialog = AlertDialog.Builder(this)
            .setTitle(R.string.dialog_disconnect_title)
            .setMessage(R.string.dialog_disconnect_message)
            .setNegativeButton(R.string.dialog_keep_sharing) { dialog, _ ->
                dialog.dismiss()
                if (videoShown) showPlaybackPanel(autoHide = true)
            }
            .setPositiveButton(R.string.dialog_disconnect_confirm) { _, _ -> backToScanning() }
            .create()
        dialog.setOnShowListener {
            dialog.getButton(AlertDialog.BUTTON_NEGATIVE).requestFocus()
        }
        dialog.show()
    }

    private fun showTransientError(message: String) {
        Toast.makeText(this, message, Toast.LENGTH_SHORT).show()
    }

    private fun deviceLabel(): String {
        val model = Build.MODEL ?: "android-tv"
        return getString(R.string.device_label_fmt, model)
    }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

    private fun setStatus(newState: UiState, message: String, paused: Boolean = false) {
        // Pertahanan: WebRTC/OkHttp bisa memanggil callback dari thread lain.
        // Menyentuh View dari non-main thread memicu
        // CalledFromWrongThreadException (dan pernah membuat proses abort
        // lewat batas JNI). Selalu pindah ke main thread dulu.
        if (android.os.Looper.myLooper() != android.os.Looper.getMainLooper()) {
            uiHandler.post { setStatus(newState, message, paused) }
            return
        }
        Log.i(TAG, "state=$newState message=$message")
        state = newState
        val friendly = when (newState) {
            UiState.SCANNING -> getString(R.string.status_ready)
            UiState.CONNECTING -> getString(R.string.status_connecting)
            UiState.CONNECTED -> when {
                paused -> getString(R.string.status_paused)
                videoShown -> getString(R.string.status_sharing)
                else -> getString(R.string.status_connected_waiting_screen)
            }
            UiState.ERROR -> getString(R.string.status_needs_check)
        }
        statusText.text = getString(R.string.status_fmt, friendly)
        setupStatusText.text = friendly
        sessionBadge.text = if (paused) "PAUSED" else "LIVE"
        val color = when (newState) {
            UiState.CONNECTED -> 0xFF52D3C6.toInt()
            UiState.ERROR -> 0xFFFF7E79.toInt()
            UiState.CONNECTING -> 0xFFF2B84B.toInt()
            UiState.SCANNING -> 0xFF52D3C6.toInt()
        }
        statusText.setTextColor(color)
        setupStatusText.setTextColor(color)
        when (newState) {
            UiState.SCANNING -> {
                setupHeadline.text = getString(R.string.setup_headline_ready)
                setupDescription.text = getString(R.string.setup_desc_ready)
            }
            UiState.CONNECTING -> {
                setupHeadline.text = getString(R.string.setup_headline_connecting)
                setupDescription.text = getString(R.string.setup_desc_connecting)
            }
            UiState.CONNECTED -> {
                setupHeadline.text = getString(R.string.setup_headline_connected)
                setupDescription.text = getString(R.string.setup_desc_connected)
            }
            UiState.ERROR -> {
                setupHeadline.text = getString(R.string.setup_headline_error)
                setupDescription.text = message
            }
        }
        if (connectedSenderLabel == null) senderText.text = getString(R.string.sender_none)
    }
}
