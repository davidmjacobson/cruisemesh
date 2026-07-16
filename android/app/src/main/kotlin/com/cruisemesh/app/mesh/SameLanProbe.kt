package com.cruisemesh.app.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** Debug-only visibility into whether two phones can make a direct TCP connection on Wi-Fi. */
enum class SameLanProbePhase {
    NO_WIFI,
    LOOKING,
    DIRECT,
    BLOCKED,
    NO_PEER,
    ERROR,
}

data class SameLanProbeSnapshot(
    val phase: SameLanProbePhase,
    val detail: String,
    val checkedAtMs: Long? = null,
)

/** Small bridge between the service-owned probe and the debug UI. */
object SameLanProbeStatus {
    var snapshot by mutableStateOf(
        SameLanProbeSnapshot(
            phase = SameLanProbePhase.NO_WIFI,
            detail = "Waiting for Wi-Fi",
        ),
    )
        private set

    @Volatile
    private var probeAgain: (() -> Unit)? = null

    internal fun registerProbeAction(action: () -> Unit) {
        probeAgain = action
    }

    internal fun unregisterProbeAction() {
        probeAgain = null
    }

    internal fun publish(value: SameLanProbeSnapshot) {
        snapshot = value
    }

    fun requestProbe() {
        probeAgain?.invoke()
    }
}

internal object SameLanProbeProtocol {
    private val magic = byteArrayOf('C'.code.toByte(), 'M'.code.toByte(), 'P'.code.toByte(), '1'.code.toByte())
    const val NONCE_SIZE = 16
    const val FRAME_SIZE = 4 + NONCE_SIZE

    fun makeFrame(nonce: ByteArray): ByteArray {
        require(nonce.size == NONCE_SIZE) { "Probe nonce must be $NONCE_SIZE bytes" }
        return magic + nonce
    }

    fun isProbeFrame(frame: ByteArray): Boolean =
        frame.size == FRAME_SIZE && frame.copyOfRange(0, magic.size).contentEquals(magic)

    fun isExpectedEcho(sent: ByteArray, received: ByteArray): Boolean =
        isProbeFrame(sent) && received.contentEquals(sent)
}

internal fun terminalProbePhase(sawPeer: Boolean, connectionFailed: Boolean): SameLanProbePhase =
    if (sawPeer || connectionFailed) SameLanProbePhase.BLOCKED else SameLanProbePhase.NO_PEER

internal fun retryDelayMs(phase: SameLanProbePhase): Long = when (phase) {
    SameLanProbePhase.NO_PEER -> 5 * 60 * 1_000L
    SameLanProbePhase.BLOCKED, SameLanProbePhase.ERROR -> 15 * 60 * 1_000L
    SameLanProbePhase.DIRECT -> 15 * 60 * 1_000L
    SameLanProbePhase.NO_WIFI, SameLanProbePhase.LOOKING -> 0L
}
