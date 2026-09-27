package com.wdt.receiver.device

import android.app.ActivityManager
import android.content.Context
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.os.Build

/**
 * Tier perangkat untuk menentukan preset streaming.
 *
 * Ambang disepakati (T5):
 * - LOW  : RAM <= 2.5 GB **atau** tidak ada hardware H.264 decoder
 * - MID  : RAM 2.5–4 GB **dan** ada hardware H.264 decoder
 * - HIGH : RAM > 4 GB **dan** ada hardware H.264 decoder
 *
 * Tier hanya dipakai untuk info/debug di T5; pengiriman ke sender
 * ditunda ke T6 (protokol v1 belum punya field capability).
 */
enum class DeviceTier {
    LOW,
    MID,
    HIGH,
}

/**
 * Hasil capability detection: RAM + dukungan hardware decoder H.264.
 *
 * @property totalRamMb total RAM perangkat dalam MiB.
 * @property hwH264Decode true bila ada decoder H.264 yang (dianggap) hardware.
 * @property hwCodecNames nama codec H.264 hardware yang terdeteksi.
 * @property tier tier hasil perhitungan.
 */
data class DeviceCapability(
    val totalRamMb: Long,
    val hwH264Decode: Boolean,
    val hwCodecNames: List<String>,
    val tier: DeviceTier,
) {
    /** Ringkasan satu baris untuk ditampilkan di layar debug. */
    fun summary(): String = buildString {
        append("tier=").append(tier)
        append(" ram=").append(totalRamMb).append("MB")
        append(" hwH264=").append(hwH264Decode)
        if (hwCodecNames.isNotEmpty()) {
            append(" [").append(hwCodecNames.joinToString(", ")).append(']')
        }
    }

    companion object {
        /** Ambang batas RAM (MiB) sesuai kesepakatan. */
        const val RAM_LOW_MAX_MB = 2_500L
        const val RAM_MID_MAX_MB = 4_000L

        private const val MIME_H264 = "video/avc"
        // Heuristik untuk API < 29 (sebelum isHardwareAccelerated tersedia):
        // penanda codec software AOSP/vendor.
        // - "google": OMX.google.* (software AOSP klasik)
        // - "c2.android": codec2 software AOSP (mis. c2.android.avc.decoder)
        // - "sw"/"soft": penanda vendor umum
        private val SOFTWARE_MARKERS = listOf("google", "c2.android", "sw", "soft")

        /** Deteksi capability perangkat saat start. */
        fun detect(context: Context): DeviceCapability {
            val totalRamMb = readTotalRamMb(context)
            val hwNames = findHardwareH264Decoders()
            val hw = hwNames.isNotEmpty()
            return DeviceCapability(
                totalRamMb = totalRamMb,
                hwH264Decode = hw,
                hwCodecNames = hwNames,
                tier = tierFor(totalRamMb, hw),
            )
        }

        internal fun tierFor(totalRamMb: Long, hwH264: Boolean): DeviceTier = when {
            !hwH264 || totalRamMb <= RAM_LOW_MAX_MB -> DeviceTier.LOW
            totalRamMb <= RAM_MID_MAX_MB -> DeviceTier.MID
            else -> DeviceTier.HIGH
        }

        private fun readTotalRamMb(context: Context): Long {
            val am = context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
            val info = ActivityManager.MemoryInfo()
            am.getMemoryInfo(info)
            return info.totalMem / (1024L * 1024L)
        }

        /**
         * Nama codec H.264 decoder yang hardware-accelerated.
         *
         * API >= 29 memakai `isHardwareAccelerated()`; API < 29 memakai
         * heuristik nama (lihat [isLikelyHardwareByName]).
         */
        private fun findHardwareH264Decoders(): List<String> {
            val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
            return list.codecInfos
                .filter { !it.isEncoder }
                .filter { info -> info.supportedTypes.any { it.equals(MIME_H264, ignoreCase = true) } }
                .filter { info -> isHardware(info) }
                .map { it.name }
        }

        private fun isHardware(info: MediaCodecInfo): Boolean =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                info.isHardwareAccelerated
            } else {
                isLikelyHardwareByName(info.name)
            }

        internal fun isLikelyHardwareByName(name: String): Boolean {
            val lower = name.lowercase()
            return SOFTWARE_MARKERS.none { lower.contains(it) }
        }
    }
}
