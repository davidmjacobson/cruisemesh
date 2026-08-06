package com.cruisemesh.app.mesh

import kotlin.math.max

/** Conservative per-phone share of a family's relay request budget. */
internal const val FAMILY_RELAY_REQUEST_INTERVAL_MS = 500L
internal const val FAMILY_RELAY_BACKOFF_BASE_MS = 1_000L
internal const val FAMILY_RELAY_BACKOFF_CAP_MS = 60_000L
internal const val FAMILY_RELAY_JITTER_WINDOW_MS = 1_000L

/** Serial request pacer; the caller performs the returned wait. */
internal class FamilyRelayRequestPacer(
    private val intervalMs: Long = FAMILY_RELAY_REQUEST_INTERVAL_MS,
) {
    private var nextRequestAtMs = 0L

    @Synchronized
    fun reserve(nowMs: Long): Long {
        val requestAtMs = max(nowMs, nextRequestAtMs)
        nextRequestAtMs = requestAtMs + intervalMs
        return requestAtMs - nowMs
    }
}

/**
 * Retry-After is a floor. Repeated 429s widen the quiet period, while a stable
 * per-identity offset keeps phones in one family from waking in lockstep.
 */
internal fun familyRelayBackoffDelayMs(
    retryAfterMs: Long,
    consecutiveRateLimits: Int,
    identityHash: Int,
): Long {
    val exponent = (consecutiveRateLimits - 1).coerceIn(0, 6)
    val exponentialMs = (FAMILY_RELAY_BACKOFF_BASE_MS shl exponent)
        .coerceAtMost(FAMILY_RELAY_BACKOFF_CAP_MS)
    val floorMs = max(retryAfterMs, exponentialMs)
    val unsignedHash = identityHash.toLong() and 0xffff_ffffL
    val jitterMs = unsignedHash % (FAMILY_RELAY_JITTER_WINDOW_MS + 1L)
    return floorMs + jitterMs
}

internal class FamilyRelayBackoff {
    var consecutiveRateLimits: Int = 0
        private set

    fun onRateLimited(retryAfterMs: Long, identityHash: Int): Long {
        consecutiveRateLimits += 1
        return familyRelayBackoffDelayMs(retryAfterMs, consecutiveRateLimits, identityHash)
    }

    fun onSuccessfulPass() {
        consecutiveRateLimits = 0
    }
}
