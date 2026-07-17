package com.cruisemesh.app.relay

/**
 * Pure exponential-backoff decision logic for [RelayPushClient]'s reconnect
 * loop. Kept free of Android/OkHttp dependencies so it's unit-testable in
 * isolation (see RelayPushBackoffTest), mirroring
 * [com.cruisemesh.app.mesh.ReconnectBackoffTracker]'s BLE reconnect backoff --
 * but simpler: a relay endpoint doesn't go stale or rotate addresses the way
 * a BLE peer does, so there is no give-up threshold here. Every dropped
 * connection just doubles the wait (capped at [maxBackoffMs]); a successful
 * connect resets it back to [initialBackoffMs]. The 60s poll
 * (`RELAY_POLL_INTERVAL_MS` in `MeshService`) is the correctness backstop no
 * matter how long this backs off, so there is nothing worth giving up into.
 */
class RelayPushBackoff(
    private val initialBackoffMs: Long = INITIAL_BACKOFF_MS,
    private val maxBackoffMs: Long = MAX_BACKOFF_MS,
) {
    private var consecutiveFailures = 0

    /** Milliseconds to wait before the next reconnect attempt, given the current failure streak. */
    fun nextDelayMs(): Long {
        val shift = consecutiveFailures.coerceAtMost(20) // avoid Long overflow on shl
        return (initialBackoffMs shl shift).coerceAtMost(maxBackoffMs)
    }

    /** Record a failed/dropped connection, growing the next [nextDelayMs] result. */
    fun recordFailure() {
        consecutiveFailures++
    }

    /** Record a successful connect; resets the backoff back to [initialBackoffMs]. */
    fun recordSuccess() {
        consecutiveFailures = 0
    }

    companion object {
        const val INITIAL_BACKOFF_MS = 2_000L
        const val MAX_BACKOFF_MS = 60_000L
    }
}
