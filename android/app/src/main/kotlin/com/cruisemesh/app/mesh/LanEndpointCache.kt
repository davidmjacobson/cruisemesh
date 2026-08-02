package com.cruisemesh.app.mesh

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.wifi.WifiInfo
import android.util.Base64
import com.cruisemesh.app.chat.UserIdHex
import java.net.Inet4Address
import uniffi.cruisemesh_core.coreLanNetworkIdForComponents
import uniffi.cruisemesh_core.coreLanNetworkIdForIpv4
import uniffi.cruisemesh_core.lanEndpointCacheIsFresh
import uniffi.cruisemesh_core.lanEndpointHostIsLocal

internal class LanEndpointCache(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(
        "cruisemesh_lan_endpoints",
        Context.MODE_PRIVATE,
    )

    fun save(
        networkId: String?,
        userId: ByteArray,
        endpoint: LanManualEndpoint,
        nowMs: Long = System.currentTimeMillis(),
    ) {
        if (networkId == null) return
        if (!lanEndpointHostIsLocal(endpoint.host)) return
        val host = Base64.encodeToString(
            endpoint.host.toByteArray(Charsets.UTF_8),
            Base64.NO_WRAP or Base64.URL_SAFE,
        )
        prefs.edit()
            .putString(key(networkId, userId), "$host|${endpoint.port}|$nowMs")
            .apply()
    }

    fun load(
        networkId: String?,
        userId: ByteArray,
        nowMs: Long = System.currentTimeMillis(),
    ): LanManualEndpoint? {
        if (networkId == null) return null
        val value = prefs.getString(key(networkId, userId), null) ?: return null
        val parts = value.split('|')
        if (parts.size != 3) return null
        val savedAt = parts[2].toLongOrNull() ?: return null
        val port = parts[1].toIntOrNull()?.takeIf { it in 1..65_535 } ?: return null
        val host = try {
            Base64.decode(parts[0], Base64.NO_WRAP or Base64.URL_SAFE)
                .toString(Charsets.UTF_8)
        } catch (_: IllegalArgumentException) {
            return null
        }
        if (!cachedLanEndpointIsDialable(host, savedAt, nowMs)) {
            prefs.edit().remove(key(networkId, userId)).apply()
            return null
        }
        return LanManualEndpoint(host, port)
    }

    private fun key(networkId: String, userId: ByteArray): String =
        "$networkId:${UserIdHex.encode(userId)}"
}

/**
 * Whether a cached endpoint may still be dialed: fresh enough, and a host the
 * shared core still considers a local network address.
 *
 * Entries written by older builds hold whatever string arrived at the time,
 * including a name, and nothing re-checked them on the way out -- so without
 * this an entry could keep a host alive for seven days after a hint stopped
 * being allowed to carry it. The core is the authority for both halves; this
 * only combines them.
 */
internal fun cachedLanEndpointIsDialable(
    host: String,
    savedAtMs: Long,
    nowMs: Long,
): Boolean = lanEndpointCacheIsFresh(savedAtMs, nowMs) && lanEndpointHostIsLocal(host)

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
