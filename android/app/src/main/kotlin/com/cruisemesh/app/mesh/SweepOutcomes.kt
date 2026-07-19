package com.cruisemesh.app.mesh

import java.net.ConnectException
import java.net.SocketException
import java.net.SocketTimeoutException

internal enum class SweepProbeOutcome {
    CONNECTED,
    REFUSED,
    TIMED_OUT,
    DENIED,
    OTHER,
}

internal data class SweepOutcomeSummary(
    val connected: Int,
    val refused: Int,
    val timedOut: Int,
    val denied: Int,
    val other: Int,
) {
    val probed: Int
        get() = connected + refused + timedOut + denied + other

    fun logLine(prefixLength: Int): String =
        "Sweep complete (/$prefixLength): $probed probed, $connected connected, " +
            "$refused refused, $timedOut timed out, $denied denied, $other other."
}

internal data class SweepOutcomeUpdate(
    val remainingCandidates: Int,
    val completedSummary: SweepOutcomeSummary?,
)

/**
 * Per-generation outcome accumulator for one subnet sweep.
 *
 * Methods are @Synchronized leaf monitors because scan workers finish in
 * parallel. A generation mismatch and any record after completion are ignored,
 * so stale workers from a torn-down sweep cannot affect its replacement and a
 * completion summary can be emitted only once.
 */
internal class SweepOutcomes(
    val generation: Int,
    private val candidateCount: Int,
) {
    private var connected = 0
    private var refused = 0
    private var timedOut = 0
    private var denied = 0
    private var other = 0
    private var completed = false

    init {
        require(candidateCount > 0)
    }

    @Synchronized
    fun record(recordGeneration: Int, outcome: SweepProbeOutcome): SweepOutcomeUpdate? {
        if (recordGeneration != generation || completed) return null
        when (outcome) {
            SweepProbeOutcome.CONNECTED -> connected++
            SweepProbeOutcome.REFUSED -> refused++
            SweepProbeOutcome.TIMED_OUT -> timedOut++
            SweepProbeOutcome.DENIED -> denied++
            SweepProbeOutcome.OTHER -> other++
        }
        val summary = snapshot()
        val remaining = (candidateCount - summary.probed).coerceAtLeast(0)
        if (remaining == 0) completed = true
        return SweepOutcomeUpdate(
            remainingCandidates = remaining,
            completedSummary = summary.takeIf { completed },
        )
    }

    @Synchronized
    fun remainingCandidates(): Int =
        if (completed) 0 else (candidateCount - snapshot().probed).coerceAtLeast(0)

    private fun snapshot() = SweepOutcomeSummary(
        connected = connected,
        refused = refused,
        timedOut = timedOut,
        denied = denied,
        other = other,
    )
}

internal enum class LanSweepVerdict {
    ISOLATION_SUSPECTED,
    BLOCKED_BY_POLICY,
    HEALTHY_BUT_EMPTY,
    FOUND_PEER,
    INCONCLUSIVE,
}

enum class LanSweepDisplayState {
    NONE,
    CHECKING,
    ISOLATION_SUSPECTED,
    BLOCKED_BY_POLICY,
}

/**
 * Android-free state holder for the LAN sweep result shown in diagnostics.
 *
 * A verdict can only enter the display state through [onSweepCompleted], whose
 * summary is emitted only after every candidate has retired. Network changes
 * and peer evidence synchronously replace any previous verdict so diagnostics
 * never describe a stale network.
 */
internal class LanSweepDisplayTracker {
    private var state = LanSweepDisplayState.NONE
    private var peerSeenOnNetwork = false

    @Synchronized
    fun onNetworkJoined(): LanSweepDisplayState {
        peerSeenOnNetwork = false
        return set(LanSweepDisplayState.CHECKING)
    }

    @Synchronized
    fun onNetworkLost(): LanSweepDisplayState {
        peerSeenOnNetwork = false
        return set(LanSweepDisplayState.NONE)
    }

    @Synchronized
    fun onSweepStarted(): LanSweepDisplayState = set(LanSweepDisplayState.CHECKING)

    @Synchronized
    fun onSweepCompleted(summary: SweepOutcomeSummary): LanSweepDisplayState {
        val next = if (peerSeenOnNetwork) {
            LanSweepDisplayState.NONE
        } else {
            when (lanSweepVerdict(summary)) {
                LanSweepVerdict.ISOLATION_SUSPECTED -> LanSweepDisplayState.ISOLATION_SUSPECTED
                LanSweepVerdict.BLOCKED_BY_POLICY -> LanSweepDisplayState.BLOCKED_BY_POLICY
                else -> LanSweepDisplayState.NONE
            }
        }
        return set(next)
    }

    @Synchronized
    fun onPeerEvidence(): LanSweepDisplayState {
        peerSeenOnNetwork = true
        return set(LanSweepDisplayState.NONE)
    }

    @Synchronized
    fun current(): LanSweepDisplayState = state

    private fun set(next: LanSweepDisplayState): LanSweepDisplayState {
        state = next
        return next
    }
}

/** Pure policy decision for the user-facing result and planner reaction. */
internal fun lanSweepVerdict(summary: SweepOutcomeSummary): LanSweepVerdict = when {
    summary.connected > 0 -> LanSweepVerdict.FOUND_PEER
    // Policy denial is more specific than all-silent isolation and must not
    // change scheduling: a VPN can deny every socket before it reaches Wi-Fi.
    summary.denied > 0 -> LanSweepVerdict.BLOCKED_BY_POLICY
    summary.probed >= MIN_ISOLATION_SWEEP_CANDIDATES && summary.refused == 0 ->
        LanSweepVerdict.ISOLATION_SUSPECTED
    summary.refused > 0 -> LanSweepVerdict.HEALTHY_BUT_EMPTY
    else -> LanSweepVerdict.INCONCLUSIVE
}

internal fun classifySweepProbeFailure(error: Exception): SweepProbeOutcome {
    val causes = generateSequence<Throwable>(error) { it.cause }.toList()
    return when {
        causes.any { it is SecurityException } -> SweepProbeOutcome.DENIED
        causes.any { it is SocketTimeoutException } -> SweepProbeOutcome.TIMED_OUT
        causes.any { it is ConnectException } -> SweepProbeOutcome.REFUSED
        causes.any { it is SocketException && it.message.indicatesEperm() } ->
            SweepProbeOutcome.DENIED
        else -> SweepProbeOutcome.OTHER
    }
}

private fun String?.indicatesEperm(): Boolean {
    val message = this ?: return false
    return message.contains("EPERM", ignoreCase = true) ||
        message.contains("operation not permitted", ignoreCase = true)
}

private const val MIN_ISOLATION_SWEEP_CANDIDATES = 253
