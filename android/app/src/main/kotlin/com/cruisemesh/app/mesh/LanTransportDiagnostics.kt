package com.cruisemesh.app.mesh

import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

data class LanTransportSnapshot(
    val state: String = "Waiting for Wi-Fi",
    val localEndpoint: String? = null,
    val lastPeerEndpoint: String? = null,
    val activePeerNames: List<String> = emptyList(),
    val lastError: String? = null,
)

internal data class LanManualEndpoint(val host: String, val port: Int) {
    val display: String
        get() = if (host.contains(':')) "[$host]:$port" else "$host:$port"
}

internal fun parseLanManualEndpoint(text: String, defaultPort: Int): LanManualEndpoint? {
    val value = text.trim()
    if (value.isEmpty()) return null

    val host: String
    val portText: String?
    if (value.startsWith("[")) {
        val closing = value.indexOf(']')
        if (closing <= 1) return null
        host = value.substring(1, closing)
        val suffix = value.substring(closing + 1)
        portText = when {
            suffix.isEmpty() -> null
            suffix.startsWith(":") && suffix.length > 1 -> suffix.substring(1)
            else -> return null
        }
    } else if (value.count { it == ':' } == 1) {
        host = value.substringBefore(':')
        portText = value.substringAfter(':')
    } else {
        // An unbracketed IPv6 address uses the default port.
        host = value
        portText = null
    }

    if (host.isBlank() || host.any(Char::isWhitespace)) return null
    val port = if (portText == null) defaultPort else portText.toIntOrNull() ?: return null
    if (port !in 1..65_535) return null
    return LanManualEndpoint(host, port)
}

/**
 * Process-wide LAN observability and the small manual-connect control surface.
 *
 * The control only asks the running transport to open a socket. The transport
 * still requires the remote Noise static key to match an accepted contact.
 */
object LanTransportDiagnostics {
    private val mutableState = MutableStateFlow(LanTransportSnapshot())
    val state: StateFlow<LanTransportSnapshot> = mutableState.asStateFlow()

    private val manualConnector =
        AtomicReference<((LanManualEndpoint) -> Unit)?>(null)
    private val activePeers = mutableMapOf<String, String>()

    fun requestManualConnection(text: String, defaultPort: Int): String? {
        val endpoint = parseLanManualEndpoint(text, defaultPort)
            ?: return "Enter an IP address or host, optionally followed by :port"
        val connector = manualConnector.get()
            ?: return "Start the mesh before connecting over local Wi-Fi"
        connector(endpoint)
        return null
    }

    internal fun registerManualConnector(connector: (LanManualEndpoint) -> Unit) {
        manualConnector.set(connector)
    }

    internal fun unregisterManualConnector() {
        manualConnector.set(null)
    }

    internal fun waitingForWifi() {
        synchronized(activePeers) { activePeers.clear() }
        mutableState.value = LanTransportSnapshot()
    }

    internal fun listening(localEndpoint: String?) {
        mutableState.update {
            it.copy(
                state = "Listening for CruiseMesh friends",
                localEndpoint = localEndpoint,
                lastError = null,
            )
        }
    }

    internal fun discovered(endpoint: String) {
        mutableState.update {
            it.copy(
                state = "Found a CruiseMesh device",
                lastPeerEndpoint = endpoint,
                lastError = null,
            )
        }
    }

    internal fun connecting(endpoint: String) {
        mutableState.update {
            it.copy(
                state = "Connecting securely",
                lastPeerEndpoint = endpoint,
                lastError = null,
            )
        }
    }

    internal fun connectionFailed(endpoint: String, reason: String) {
        mutableState.update {
            it.copy(
                state = if (it.activePeerNames.isEmpty()) "Local Wi-Fi is ready"
                else "Secure local Wi-Fi link active",
                lastPeerEndpoint = endpoint,
                lastError = reason,
            )
        }
    }

    internal fun authenticated(address: String, peerName: String) {
        val names = synchronized(activePeers) {
            activePeers[address] = peerName
            activePeers.values.distinct().sorted()
        }
        mutableState.update {
            it.copy(
                state = "Secure local Wi-Fi link active",
                activePeerNames = names,
                lastError = null,
            )
        }
    }

    internal fun disconnected(address: String) {
        val names = synchronized(activePeers) {
            activePeers.remove(address)
            activePeers.values.distinct().sorted()
        }
        mutableState.update {
            it.copy(
                state = if (names.isEmpty()) "Listening for CruiseMesh friends"
                else "Secure local Wi-Fi link active",
                activePeerNames = names,
            )
        }
    }
}
