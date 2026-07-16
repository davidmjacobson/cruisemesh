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
    val lastProbeLatencyMs: Long? = null,
    val probeStatus: String? = null,
    val sentFrames: Long = 0,
    val receivedFrames: Long = 0,
    val lastActivityAtMs: Long? = null,
    val scanProgress: Int? = null,
    val scanTotal: Int? = null,
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
    private val pendingManualEndpoint = AtomicReference<LanManualEndpoint?>(null)
    private val probeRequester = AtomicReference<(() -> String?)?>(null)
    private val scanRequester = AtomicReference<(() -> String?)?>(null)
    private val activePeers = mutableMapOf<String, String>()

    fun requestManualConnection(text: String, defaultPort: Int): String? {
        val endpoint = parseLanManualEndpoint(text, defaultPort)
            ?: return "Enter an IP address or host, optionally followed by :port"
        val connector = manualConnector.get()
            ?: return "Start the mesh before connecting over local Wi-Fi"
        connector(endpoint)
        return null
    }

    fun requestConnectionTest(): String? {
        val requester = probeRequester.get()
            ?: return "Start the mesh before testing local Wi-Fi"
        return requester()
    }

    fun requestSubnetScan(): String? {
        val requester = scanRequester.get()
            ?: return "Start the mesh before searching the local subnet"
        return requester()
    }

    internal fun registerManualConnector(connector: (LanManualEndpoint) -> Unit) {
        manualConnector.set(connector)
        pendingManualEndpoint.getAndSet(null)?.let(connector)
    }

    internal fun unregisterManualConnector() {
        manualConnector.set(null)
    }

    internal fun queueManualConnection(endpoint: LanManualEndpoint) {
        val connector = manualConnector.get()
        if (connector != null) {
            connector(endpoint)
        } else {
            pendingManualEndpoint.set(endpoint)
        }
    }

    internal fun registerProbeRequester(requester: () -> String?) {
        probeRequester.set(requester)
    }

    internal fun unregisterProbeRequester() {
        probeRequester.set(null)
    }

    internal fun registerScanRequester(requester: () -> String?) {
        scanRequester.set(requester)
    }

    internal fun unregisterScanRequester() {
        scanRequester.set(null)
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
                probeStatus = null,
            )
        }
    }

    internal fun discovered(endpoint: String) {
        mutableState.update {
            it.copy(
                state = "Found a CruiseMesh device",
                lastPeerEndpoint = endpoint,
                lastError = null,
                probeStatus = null,
            )
        }
    }

    internal fun connecting(endpoint: String) {
        mutableState.update {
            it.copy(
                state = "Connecting securely",
                lastPeerEndpoint = endpoint,
                lastError = null,
                probeStatus = null,
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
                probeStatus = null,
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
                probeStatus = null,
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

    internal fun probeStarted() {
        mutableState.update {
            it.copy(
                probeStatus = "Testing encrypted LAN link…",
                lastError = null,
            )
        }
    }

    internal fun probeSucceeded(latencyMs: Long) {
        mutableState.update {
            it.copy(
                probeStatus = "Encrypted round trip: ${latencyMs} ms",
                lastProbeLatencyMs = latencyMs,
                lastError = null,
                lastActivityAtMs = System.currentTimeMillis(),
            )
        }
    }

    internal fun probeFailed(reason: String) {
        mutableState.update {
            it.copy(
                probeStatus = null,
                lastError = reason,
            )
        }
    }

    internal fun frameSent() {
        mutableState.update {
            it.copy(
                sentFrames = it.sentFrames + 1,
                lastActivityAtMs = System.currentTimeMillis(),
            )
        }
    }

    internal fun frameReceived() {
        mutableState.update {
            it.copy(
                receivedFrames = it.receivedFrames + 1,
                lastActivityAtMs = System.currentTimeMillis(),
            )
        }
    }

    internal fun scanStarted(total: Int) {
        mutableState.update {
            it.copy(
                scanProgress = 0,
                scanTotal = total,
                lastError = null,
            )
        }
    }

    internal fun scanAdvanced() {
        mutableState.update {
            val total = it.scanTotal ?: return@update it
            val progress = ((it.scanProgress ?: 0) + 1).coerceAtMost(total)
            it.copy(
                scanProgress = if (progress == total) null else progress,
                scanTotal = if (progress == total) null else total,
                probeStatus = if (progress == total) "Local /24 search finished" else it.probeStatus,
            )
        }
    }
}
