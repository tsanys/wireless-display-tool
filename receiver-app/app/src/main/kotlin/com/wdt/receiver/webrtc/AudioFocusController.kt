package com.wdt.receiver.webrtc

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.util.Log

/**
 * Abstraksi permintaan/pelepasan audio focus — memisahkan logika dari
 * `AudioManager` agar dapat diuji di JVM tanpa device.
 */
interface AudioFocusSink {
    /** Minta focus media (playback TV). */
    fun requestMediaFocus()

    /** Lepaskan focus (wajib saat stop/disable/teardown). */
    fun abandonMediaFocus()
}

/**
 * Pengelola audio focus: idempoten dan selalu melepas focus bila tidak lagi
 * aktif sehingga tidak ada focus tersisa setelah sesi berhenti.
 */
class AudioFocusController(private val sink: AudioFocusSink) {
    private var active = false

    /** Aktifkan/nonaktifkan focus; hanya bertindak saat status berubah. */
    fun setEnabled(enabled: Boolean) {
        if (enabled == active) return
        active = enabled
        if (enabled) {
            sink.requestMediaFocus()
        } else {
            sink.abandonMediaFocus()
        }
    }

    fun isActive(): Boolean = active

    /** Pastikan focus dilepas (dipanggil saat close/teardown). */
    fun release() = setEnabled(false)
}

/**
 * Implementasi Android: media playback focus (USAGE_MEDIA / CONTENT_TYPE_MOVIE)
 * dengan AUDIOFOCUS_GAIN, dibiarkan duck-free (TV adalah output utama).
 */
class AndroidAudioFocusSink(context: Context) : AudioFocusSink {
    private val audioManager =
        context.applicationContext.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private var request: AudioFocusRequest? = null

    override fun requestMediaFocus() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                        .build(),
                )
                .setOnAudioFocusChangeListener { change ->
                    Log.i(TAG, "audioFocusChange=$change")
                }
                .build()
            request = req
            val result = audioManager.requestAudioFocus(req)
            Log.i(TAG, "requestAudioFocus=${result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED}")
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(
                null,
                AudioManager.STREAM_MUSIC,
                AudioManager.AUDIOFOCUS_GAIN,
            )
            Log.i(TAG, "requestAudioFocus (legacy)")
        }
    }

    override fun abandonMediaFocus() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            request?.let { audioManager.abandonAudioFocusRequest(it) }
        } else {
            @Suppress("DEPRECATION")
            audioManager.abandonAudioFocus(null)
        }
        request = null
        Log.i(TAG, "abandonAudioFocus")
    }

    private companion object {
        const val TAG = "WdtAudioFocus"
    }
}
