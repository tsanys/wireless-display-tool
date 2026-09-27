package com.wdt.receiver.discovery

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * Sender WDT yang ditemukan via mDNS/NSD.
 *
 * @property instanceName nama instance mDNS, mis. "WDT MacBook-Pro".
 * @property host alamat IP sender (hostAddress).
 * @property port port signaling server.
 * @property proto TXT `proto` bila ada (versi protokol).
 * @property version TXT `ver` bila ada (versi sender app).
 */
data class SenderInfo(
    val instanceName: String,
    val host: String,
    val port: Int,
    val proto: Int?,
    val version: String?,
)

/**
 * NSD discovery untuk menemukan sender WDT di LAN.
 *
 * Service type mengikuti `core/src/signaling/protocol.rs`
 * (`_wdt._tcp.local.`); Android memakai bentuk `_wdt._tcp`.
 *
 * Resolve dilakukan satu per satu (NsdManager tidak mengizinkan resolve
 * bersamaan) lewat antrean internal. MulticastLock diambil selama
 * discovery agar mDNS andal di device TV.
 */
class NsdDiscovery(
    context: Context,
    private val onSendersChanged: (List<SenderInfo>) -> Unit,
    private val onError: (String) -> Unit = {},
) {
    private val appContext = context.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val nsdManager =
        appContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val wifiManager =
        appContext.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager

    private var multicastLock: WifiManager.MulticastLock? = null
    private var discovering = false

    /** Sender yang sudah ter-resolve, key = instanceName. */
    private val resolved = LinkedHashMap<String, SenderInfo>()

    /** instanceName yang menunggu resolve (antrean serial). */
    private val pendingResolve = ArrayDeque<String>()

    /** serviceInfo mentah per instanceName (untuk menandai hilang). */
    private val rawServices = LinkedHashMap<String, NsdServiceInfo>()
    private var resolving = false

    private val discoveryListener = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(serviceType: String) {
            Log.d(TAG, "discovery started: $serviceType")
        }

        override fun onServiceFound(service: NsdServiceInfo) {
            val name = service.serviceName ?: return
            rawServices[name] = service
            pendingResolve.addLast(name)
            resolveNext()
        }

        override fun onServiceLost(service: NsdServiceInfo) {
            val name = service.serviceName ?: return
            rawServices.remove(name)
            if (resolved.remove(name) != null) {
                publish()
            }
        }

        override fun onDiscoveryStopped(serviceType: String) {
            Log.d(TAG, "discovery stopped: $serviceType")
        }

        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
            Log.e(TAG, "start discovery failed: $errorCode")
            discovering = false
            releaseLock()
            postError("NSD discovery gagal start (kode $errorCode)")
        }

        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
            Log.e(TAG, "stop discovery failed: $errorCode")
            runCatching { nsdManager.stopServiceDiscovery(this) }
        }
    }

    private val resolveListener = object : NsdManager.ResolveListener {
        override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            Log.w(TAG, "resolve failed ${serviceInfo.serviceName}: $errorCode")
            resolving = false
            resolveNext()
        }

        override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
            resolving = false
            val name = serviceInfo.serviceName ?: run {
                resolveNext()
                return
            }
            val host = resolveHostAddress(serviceInfo)
            val port = serviceInfo.port
            if (host == null || port <= 0) {
                resolveNext()
                return
            }
            resolved[name] = SenderInfo(
                instanceName = name,
                host = host,
                port = port,
                proto = txtString(serviceInfo, "proto")?.toIntOrNull(),
                version = txtString(serviceInfo, "ver"),
            )
            publish()
            resolveNext()
        }
    }

    /** Mulai discovery; idempoten. */
    fun start() {
        if (discovering) return
        acquireLock()
        resolved.clear()
        rawServices.clear()
        pendingResolve.clear()
        resolving = false
        runCatching {
            nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discoveryListener)
            discovering = true
        }.onFailure {
            Log.e(TAG, "discoverServices gagal", it)
            releaseLock()
            postError("NSD discovery gagal: ${it.message}")
        }
    }

    /** Hentikan discovery dan lepas lock; idempoten. */
    fun stop() {
        if (discovering) {
            runCatching { nsdManager.stopServiceDiscovery(discoveryListener) }
            discovering = false
        }
        releaseLock()
    }

    private fun resolveNext() {
        if (resolving) return
        val nextName = pendingResolve.removeFirstOrNull() ?: return
        val service = rawServices[nextName] ?: run {
            resolveNext()
            return
        }
        resolving = true
        // resolveService deprecated di API 34 (diganti registerServiceInfoCallback),
        // tapi tetap didukung dan merupakan satu-satunya jalur untuk API < 34
        // (baseline device Android 11 / API 30).
        @Suppress("DEPRECATION")
        runCatching { nsdManager.resolveService(service, resolveListener) }
            .onFailure {
                Log.w(TAG, "resolveService gagal untuk $nextName", it)
                resolving = false
            }
    }

    private fun publish() {
        val snapshot = resolved.values.toList()
        main.post { onSendersChanged(snapshot) }
    }

    private fun postError(message: String) {
        main.post { onError(message) }
    }

    private fun acquireLock() {
        val lock = wifiManager
            ?.createMulticastLock("wdt-nsd")
            ?.apply { setReferenceCounted(true) }
        runCatching {
            lock?.acquire()
            multicastLock = lock
        }.onFailure { Log.w(TAG, "multicast lock gagal", it) }
    }

    private fun releaseLock() {
        runCatching {
            multicastLock?.takeIf { it.isHeld }?.release()
        }
        multicastLock = null
    }

    /** Baca TXT record sebagai String. */
    private fun txtString(info: NsdServiceInfo, key: String): String? {
        val bytes = info.attributes?.get(key) ?: return null
        return runCatching { String(bytes, Charsets.UTF_8) }.getOrNull()
    }

    /**
     * Ambil alamat host service.
     *
     * API 34+ memakai `hostAddresses` (menggantikan `host` yang deprecated);
     * API < 34 memakai `host`. Keduanya mengembalikan alamat pertama yang
     * tersedia agar tetap kompatibel dengan device baseline Android 11.
     */
    private fun resolveHostAddress(info: NsdServiceInfo): String? {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            info.hostAddresses?.firstOrNull()?.hostAddress?.let { return it }
        }
        @Suppress("DEPRECATION")
        return info.host?.hostAddress
    }

    companion object {
        private const val TAG = "NsdDiscovery"

        /** Sama dengan MDNS_SERVICE_TYPE di core (bentuk Android tanpa domain). */
        const val SERVICE_TYPE = "_wdt._tcp"
    }
}
