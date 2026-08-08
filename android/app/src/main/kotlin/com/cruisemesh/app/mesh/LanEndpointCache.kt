package com.cruisemesh.app.mesh

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.wifi.WifiInfo
import com.cruisemesh.app.chat.UserIdHex
import java.net.Inet4Address
import uniffi.cruisemesh_core.LanEndpointCacheDecision
import uniffi.cruisemesh_core.LanEndpointCacheEntry
import uniffi.cruisemesh_core.LanEndpointProvenance
import uniffi.cruisemesh_core.coreLanNetworkIdForComponents
import uniffi.cruisemesh_core.coreLanNetworkIdForIpv4
import uniffi.cruisemesh_core.lanEndpointCacheDecision
import uniffi.cruisemesh_core.lanEndpointCacheDecode
import uniffi.cruisemesh_core.lanEndpointCacheEncodeUpdate
import uniffi.cruisemesh_core.lanEndpointHostIsLocal

/**
 * Per-network memory of where a contact was last reachable over Wi-Fi.
 *
 * Every entry records how the address became known -- see
 * [LanEndpointProvenance]. The shared core owns the stored format and the
 * rules; this class only reaches SharedPreferences.
 */
internal class LanEndpointCache(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(
        "cruisemesh_lan_endpoints",
        Context.MODE_PRIVATE,
    )

    /** Serialises the read-modify-write in [save] and the read-then-delete in
     * [load]; see [save] for why. */
    private val writeLock = Any()

    fun save(
        networkId: String?,
        userId: ByteArray,
        endpoint: LanManualEndpoint,
        provenance: LanEndpointProvenance,
        nowMs: Long = System.currentTimeMillis(),
    ) {
        if (networkId == null) return
        if (!lanEndpointHostIsLocal(endpoint.host)) return
        // The stored port is a u16 and [Int.toUShort] truncates rather than
        // refusing, so an out-of-range value would be filed as some unrelated
        // port instead of being dropped. No caller can produce one today; the
        // check is here so none can start.
        if (endpoint.port !in 1..65_535) return
        val key = key(networkId, userId)
        val entry = LanEndpointCacheEntry(
            host = endpoint.host,
            port = endpoint.port.toUShort(),
            savedAtMs = nowMs,
            provenance = provenance,
        )
        // A read-modify-write, and saves arrive on at least three threads: the
        // LAN handler when an outbound dial is observed, the store executor
        // when a hint lands, the connection executor when a handshake
        // completes. Unlocked, a contact's periodic hint can read the
        // pre-promotion value, the handshake can write "authenticated", and
        // the hint can then overwrite it with "hinted" -- demoting a proven
        // address, which is the one thing [lanEndpointCacheEncodeUpdate]
        // exists to prevent, and enough to evict a working routed-LAN peer on
        // the next Wi-Fi join. [SharedPreferences.Editor.apply] publishes to
        // the in-memory map before it returns, so holding the lock across the
        // read and the edit makes the pair atomic.
        synchronized(writeLock) {
            prefs.edit()
                .putString(key, lanEndpointCacheEncodeUpdate(prefs.getString(key, null), entry))
                .apply()
        }
    }

    /**
     * The cached endpoint for this contact on this network, if one may still
     * be dialed. [localHost] is this phone's own LAN address, which is what
     * lets an unproven entry be checked against the network we are actually
     * on; an entry the core rules out for good is deleted here rather than
     * left to age out over seven days.
     */
    fun load(
        networkId: String?,
        userId: ByteArray,
        localHost: String?,
        nowMs: Long = System.currentTimeMillis(),
    ): LanManualEndpoint? {
        if (networkId == null) return null
        val key = key(networkId, userId)
        // Under the same lock as [save]: a delete decided from a value read
        // before a concurrent save would otherwise throw the save away.
        return synchronized(writeLock) {
            val value = prefs.getString(key, null) ?: return@synchronized null
            val entry = lanEndpointCacheDecode(value)
            if (entry == null) {
                prefs.edit().remove(key).apply()
                return@synchronized null
            }
            when (lanEndpointCacheDecision(entry, localHost, nowMs)) {
                LanEndpointCacheDecision.USE -> LanManualEndpoint(entry.host, entry.port.toInt())
                LanEndpointCacheDecision.SKIP -> null
                LanEndpointCacheDecision.EVICT -> {
                    prefs.edit().remove(key).apply()
                    null
                }
            }
        }
    }

    private fun key(networkId: String, userId: ByteArray): String =
        "$networkId:${UserIdHex.encode(userId)}"
}

/**
 * Best-effort, permission-free network fingerprint. SSID is used when the OS
 * exposes it. The IPv4 /24 is the canonical cross-platform input so Android
 * and iOS derive the same identifier without requiring SSID permission. DNS
 * topology is a weaker fallback. Only a truncated hash is persisted.
 */
internal fun lanNetworkId(
    connectivityManager: ConnectivityManager,
    network: Network,
): String? {
    val capabilities = connectivityManager.getNetworkCapabilities(network)
    val wifiInfo = capabilities?.transportInfo as? WifiInfo
    @Suppress("DEPRECATION")
    val ssid = wifiInfo?.ssid
        ?.takeUnless { it == "<unknown ssid>" || it.isBlank() }
    val link = connectivityManager.getLinkProperties(network)
    val ipv4 = link?.linkAddresses
        ?.mapNotNull { it.address as? Inet4Address }
        ?.firstOrNull()
    ipv4?.hostAddress?.let { return lanNetworkIdForIpv4(it) }
    val topology = buildList {
        ssid?.let { add("ssid:$it") }
        link?.dnsServers
            ?.map { it.hostAddress.orEmpty() }
            ?.sorted()
            ?.forEach { add("dns:$it") }
        link?.domains?.takeIf(String::isNotBlank)?.let { add("domains:$it") }
    }
    if (topology.isEmpty()) return null
    return coreLanNetworkIdForComponents(topology)
}

internal fun lanNetworkIdForIpv4(address: String): String? {
    return coreLanNetworkIdForIpv4(address)
}
