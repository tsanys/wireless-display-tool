package com.wdt.receiver.webrtc

import android.util.Log
import org.webrtc.VideoFrame
import org.webrtc.VideoSink
import kotlin.math.min

/**
 * Probe debug T6: mengukur "usia" frame tepat di sink (setelah decode, sebelum
 * render) tanpa screenshot. Frame video membawa strip jam biner (lihat
 * /tmp/wdt_clock.html): 12 sel (sel 0 & 11 marker putih; sel 1..10 = 10 bit
 * nilai milidetik jam sender). Probe men-decode nilai itu dari plane Y, lalu
 * `age = (jamDeviceMs - nilai) mod 1000`.
 *
 * Frame diteruskan apa adanya ke [target] (renderer), jadi tidak mengubah
 * perilaku streaming.
 *
 * Catatan: `age` masih memuat offset jam sender↔device (Δ) yang sama dengan
 * metode jam device; koreksi Δ dipakai saat interpretasi.
 */
class LatencyProbeSink(
    private val target: VideoSink,
    private val tag: String = "WdtProbe",
) : VideoSink {

    // Aspek layar sumber (laptop macOS 1800x1169) — dipakai untuk memetakan
    // posisi strip dari layar ke frame hasil komposisi (letterbox).
    private val srcW = 1800.0
    private val srcH = 1169.0

    private val window = ArrayDeque<Int>()
    private var lastLogMs = 0L

    override fun onFrame(frame: VideoFrame) {
        val d = decodeStrip(frame)
        if (d != null) {
            val age = ((System.currentTimeMillis() % 1000L).toInt() - d + 1000) % 1000
            window.addLast(age)
            if (window.size > 180) window.removeFirst()
            val now = System.currentTimeMillis()
            if (now - lastLogMs >= 1000L && window.size >= 10) {
                lastLogMs = now
                val s = window.sorted()
                Log.i(
                    tag,
                    "age@idle median=${s[s.size / 2]}ms min=${s.first()} max=${s.last()} " +
                        "n=${s.size} D=$d nowMs=${now % 1000}",
                )
            }
        }
        target.onFrame(frame)
    }

    private fun decodeStrip(frame: VideoFrame): Int? {
        val b = frame.buffer
        val created = b !is VideoFrame.I420Buffer
        val i420: VideoFrame.I420Buffer =
            try {
                (if (b is VideoFrame.I420Buffer) b else b.toI420()) ?: return null
            } catch (_: Throwable) {
                return null
            }
        try {
            val w = i420.width
            val h = i420.height
            if (w < 64 || h < 32) return null
            val scale = min(w / srcW, h / srcH)
            val innerW = ((srcW * scale).toInt()) and -2
            val innerH = ((srcH * scale).toInt()) and -2
            if (innerW < 48 || innerH < 24) return null
            val offX = (w - innerW) / 2
            val offY = (h - innerH) / 2
            val left = offX + (0.10 * innerW).toInt()
            val span = (0.80 * innerW).toInt()
            val yc = offY + innerH / 2
            val y = i420.dataY
            val stride = i420.strideY

            fun cell(k: Int): Int {
                val cx = left + ((k + 0.5) * span / 12.0).toInt()
                var sum = 0
                var n = 0
                var dy = -2
                while (dy <= 2) {
                    var dx = -2
                    while (dx <= 2) {
                        val xx = (cx + dx).coerceIn(0, w - 1)
                        val yy = (yc + dy).coerceIn(0, h - 1)
                        sum += y.get(yy * stride + xx).toInt() and 0xFF
                        n++
                        dx++
                    }
                    dy++
                }
                return sum / n
            }

            if (cell(0) < 128 || cell(11) < 128) return null // marker hilang
            var v = 0
            for (k in 1..10) v = (v shl 1) or if (cell(k) > 128) 1 else 0
            return v
        } finally {
            if (created) runCatching { i420.release() }
        }
    }
}
