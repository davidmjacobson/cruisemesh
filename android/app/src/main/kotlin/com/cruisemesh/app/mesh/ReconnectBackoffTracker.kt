package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreReconnectBackoffTracker

class ReconnectBackoffTracker(
    initialBackoffMs: Long = INITIAL_BACKOFF_MS,
    maxBackoffMs: Long = MAX_BACKOFF_MS,
    maxConsecutiveFailures: Int = MAX_CONSECUTIVE_FAILURES,
    giveUpProbeMs: Long = GIVE_UP_PROBE_MS,
) {
    private val core =
        CoreReconnectBackoffTracker(initialBackoffMs, maxBackoffMs, maxConsecutiveFailures.toUInt(), giveUpProbeMs)
    fun canAttempt(address: String, nowMs: Long): Boolean = core.canAttempt(address, nowMs)
    fun isGivenUp(address: String): Boolean = core.isGivenUp(address)
    fun failureCount(address: String): Int = core.failureCount(address).toInt()
    fun retryDelayMs(address: String, nowMs: Long): Long? = core.retryDelayMs(address, nowMs)
    fun recordFailure(address: String, nowMs: Long): Int = core.recordFailure(address, nowMs).toInt()
    fun recordSuccess(address: String) = core.recordSuccess(address)
    fun clear() = core.clear()

    companion object {
        const val INITIAL_BACKOFF_MS = 5_000L
        const val MAX_BACKOFF_MS = 5 * 60_000L
        const val MAX_CONSECUTIVE_FAILURES = 6

        // Give-up is a slow probe cadence, never a permanent refusal: transient
        // GATT failures during a Wi-Fi teardown can exhaust the failure budget
        // against a peer's still-current advertisement address (RPAs rotate
        // only every ~15 min), which used to wedge the link on both sides
        // until Bluetooth was cycled (observed live 2026-07-24).
        const val GIVE_UP_PROBE_MS = 60_000L
    }
}
