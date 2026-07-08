package com.cruisemesh.app.mesh

/**
 * Per-address exponential backoff for [BleCentral]'s reconnect attempts.
 *
 * Kept as a plain class with no Android framework dependencies so it's
 * unit-testable in isolation (see ReconnectBackoffTrackerTest).
 *
 * Each address starts eligible immediately. Every consecutive failure to
 * fully connect doubles the required wait (from [initialBackoffMs] up to
 * [maxBackoffMs]) before that address may be tried again. After
 * [maxConsecutiveFailures] consecutive failures, the address is considered
 * given up -- it's likely a stale or rotated BLE address that no longer
 * corresponds to a reachable peer -- and [canAttempt] refuses it until
 * [recordSuccess] is called for it.
 *
 * Because state is keyed by address, a peer that rotates its advertising
 * address is rediscovered under a brand-new key with no prior failure
 * history: giving up on a stale address never blocks a fresh one. Callers
 * only need to key their scan-result handling by address, as [BleCentral]
 * already does.
 */
class ReconnectBackoffTracker(
    private val initialBackoffMs: Long = INITIAL_BACKOFF_MS,
    private val maxBackoffMs: Long = MAX_BACKOFF_MS,
    private val maxConsecutiveFailures: Int = MAX_CONSECUTIVE_FAILURES,
) {
    private data class State(val consecutiveFailures: Int, val nextEligibleAtMs: Long)

    private val state = mutableMapOf<String, State>()

    /** True if [address] is eligible for a connection attempt at [nowMs]. */
    fun canAttempt(address: String, nowMs: Long): Boolean {
        val s = state[address] ?: return true
        if (s.consecutiveFailures >= maxConsecutiveFailures) return false
        return nowMs >= s.nextEligibleAtMs
    }

    /** True once [address] has exceeded the consecutive-failure budget. */
    fun isGivenUp(address: String): Boolean =
        (state[address]?.consecutiveFailures ?: 0) >= maxConsecutiveFailures

    /** Number of consecutive failures recorded for [address] so far. */
    fun failureCount(address: String): Int = state[address]?.consecutiveFailures ?: 0

    /**
     * Record a failed/aborted connection attempt (or a disconnect) for
     * [address] at [nowMs], scheduling its next eligible attempt time.
     * Returns the resulting consecutive-failure count.
     */
    fun recordFailure(address: String, nowMs: Long): Int {
        val previousFailures = state[address]?.consecutiveFailures ?: 0
        val failures = previousFailures + 1
        val shift = (failures - 1).coerceAtMost(20) // avoid Long overflow on shl
        val backoff = (initialBackoffMs shl shift).coerceAtMost(maxBackoffMs)
        state[address] = State(failures, nowMs + backoff)
        return failures
    }

    /** Record a fully successful connection to [address]; clears its backoff state. */
    fun recordSuccess(address: String) {
        state.remove(address)
    }

    companion object {
        const val INITIAL_BACKOFF_MS = 5_000L
        const val MAX_BACKOFF_MS = 5 * 60_000L // 5 minutes
        const val MAX_CONSECUTIVE_FAILURES = 6
    }
}
