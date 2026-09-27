package com.wdt.receiver.pairing

import com.wdt.receiver.signaling.Protocol

/**
 * Info pairing hasil input manual.
 *
 * @property host alamat IP sender.
 * @property port port signaling.
 * @property token token pairing 6-digit.
 */
data class PairingInfo(
    val host: String,
    val port: Int,
    val token: String,
) {
    /** URL WebSocket signaling untuk info ini. */
    fun wsUrl(): String = "ws://$host:$port${Protocol.WS_PATH}"

    /** String kanonik sesuai format dokumen (untuk verifikasi/echo). */
    fun canonical(): String = "$host:$port:$token"
}

/** Hasil parsing input pairing manual. */
sealed interface PairingResult {
    data class Success(val info: PairingInfo) : PairingResult
    data class Failure(val reason: String) : PairingResult
}

/**
 * Parser fallback manual `ip:port:token` (format final
 * `docs/SIGNALING_PROTOCOL.md`: `192.168.1.5:8420:123456`).
 *
 * Toleran terhadap spasi di ujung; token wajib 6 digit (sesuai generator
 * `%06d` sender di T3).
 */
object ManualPairing {
    private const val TOKEN_LENGTH = 6

    /**
     * Token saja (6 digit) — dipakai saat host/port sudah diketahui dari
     * discovery NSD, jadi user cukup mengetik token.
     */
    fun parseTokenOnly(raw: String): String? {
        val t = raw.trim()
        return if (t.length == TOKEN_LENGTH && t.all(Char::isDigit)) t else null
    }

    fun parse(raw: String): PairingResult {
        val trimmed = raw.trim()
        if (trimmed.isEmpty()) return PairingResult.Failure("input kosong")

        val parts = trimmed.split(':')
        if (parts.size != 3) {
            return PairingResult.Failure("format harus ip:port:token (mis. 192.168.1.5:8420:123456)")
        }

        val host = parts[0].trim()
        val portStr = parts[1].trim()
        val token = parts[2].trim()

        if (host.isEmpty()) return PairingResult.Failure("IP/host kosong")
        if (!isValidHost(host)) return PairingResult.Failure("IP/host tidak valid: $host")

        val port = portStr.toIntOrNull()
            ?: return PairingResult.Failure("port bukan angka: $portStr")
        if (port !in 1..65535) return PairingResult.Failure("port di luar rentang 1-65535: $port")

        if (token.length != TOKEN_LENGTH || !token.all { it.isDigit() }) {
            return PairingResult.Failure("token harus $TOKEN_LENGTH digit angka")
        }

        return PairingResult.Success(PairingInfo(host = host, port = port, token = token))
    }

    /** IPv4 valid, atau hostname non-kosong tanpa spasi. */
    private fun isValidHost(host: String): Boolean {        if (host.contains(' ')) return false
        val octets = host.split('.')
        val looksLikeIpv4 = octets.size == 4 && octets.all { it.all(Char::isDigit) && it.isNotEmpty() }
        if (!looksLikeIpv4) {
            // Bukan bentuk IPv4 → terima sebagai hostname selama ada titik/karakter valid.
            return host.isNotBlank()
        }
        return octets.all { it.toIntOrNull()?.let { v -> v in 0..255 } == true }
    }
}
