package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreFamilyRelayBackoff
import uniffi.cruisemesh_core.CoreFamilyRelayPacer

/**
 * Delegating shims over the family relay backpressure policy, which lives in
 * the core (`core/src/session/relay_policy.rs`).
 *
 * A CruiseMesh family shares one relay request budget, so how fast a phone may
 * ask and what it does when the relay says "too fast" are protocol decisions,
 * not Android decisions. This file used to hold the interval, the exponential
 * curve, the cap, the jitter window and the arithmetic joining them, with a
 * second copy of all of it in Swift. It now holds none of that: no constant,
 * no formula, no branch. What is left is the shape Android's sync engine calls
 * -- a class it can hold as a field, and Kotlin's `Long` where the core speaks
 * `ULong`.
 *
 * The one behaviour change the hoist carried is the jitter input. This shell
 * used to seed the per-phone offset from `ByteArray.contentHashCode()`, a
 * platform hash that iOS could not possibly agree with (it used its own
 * FNV-1a). The core derives it from the public user id under a documented
 * BLAKE2b context instead, so both shells draw from one function -- see
 * `RATE-01` in specs/protocol-contract-v1.md.
 *
 * Deliberately still here rather than deleted: removing the wrappers and
 * calling core straight from `RelaySyncEngine` is a separate step, gated on
 * paired-platform canary evidence.
 */

/** Serial request pacer; the caller performs the returned wait. */
internal class FamilyRelayRequestPacer {
    private val core = CoreFamilyRelayPacer()

    /**
     * @param nowMs a MONOTONIC reading (`SystemClock.elapsedRealtime()`), not
     *   wall clock: a pacer that can be rewound by a time correction would
     *   hand out a wait as long as the correction.
     */
    fun reserve(nowMs: Long): Long = core.reserve(nowMs)
}

/** Consecutive-429 counter and the quiet window each refusal earns. */
internal class FamilyRelayBackoff {
    private val core = CoreFamilyRelayBackoff()

    val consecutiveRateLimits: Int
        get() = core.consecutiveRateLimits().toInt()

    /**
     * @param retryAfterMs the already-clamped advertised window from
     *   `relayRetryAfterMs`, never a raw header value.
     * @param identityPublicBytes this device's public user id, which is what
     *   the core's stable anti-lockstep offset is derived from. Public on
     *   purpose: the offset is observable in request timing.
     */
    fun onRateLimited(retryAfterMs: Long, identityPublicBytes: ByteArray): Long =
        core.onRateLimited(retryAfterMs.coerceAtLeast(0L).toULong(), identityPublicBytes).toLong()

    fun onSuccessfulPass() = core.onSuccessfulPass()
}
