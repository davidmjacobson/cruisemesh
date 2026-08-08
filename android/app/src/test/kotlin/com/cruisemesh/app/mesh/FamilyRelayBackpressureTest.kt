package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.coreFamilyRelayBackoffDelayMs
import uniffi.cruisemesh_core.coreFamilyRelayBackoffVectors
import uniffi.cruisemesh_core.coreFamilyRelayJitterMs
import uniffi.cruisemesh_core.coreFamilyRelayJitterVectors
import uniffi.cruisemesh_core.coreFamilyRelayPacerVectors

/**
 * The pacing and 429 backoff policy lives in the core
 * (`core/src/session/relay_policy.rs`), and its formulas are pinned there. What
 * this file proves is the other half: that Android reaches that policy through
 * the FFI and gets the same answers back.
 *
 * So every expectation here comes from a vector table the core exports rather
 * than from a number typed into this file. The Rust suite and the Swift XCTest
 * suite assert the same tables, so a byte-array or integer-width bug in either
 * binding shows up as a vector mismatch instead of as a platform test that
 * quietly asserts something slightly different.
 */
class FamilyRelayBackpressureTest {

    @Test
    fun `the shim pacer reproduces the core's reservation sequence`() {
        // One pacer, rows applied in order: the state carried between them is
        // the point, and it is state held on the far side of the FFI.
        val pacer = FamilyRelayRequestPacer()
        for (vector in coreFamilyRelayPacerVectors()) {
            assertEquals(vector.name, vector.expectedWaitMs, pacer.reserve(vector.nowMs))
        }
    }

    @Test
    fun `the backoff curve crosses the FFI unchanged`() {
        for (vector in coreFamilyRelayBackoffVectors()) {
            assertEquals(
                vector.name,
                vector.expectedDelayMs,
                coreFamilyRelayBackoffDelayMs(
                    vector.retryAfterMs,
                    vector.consecutiveRateLimits,
                    vector.jitterMs,
                ),
            )
        }
    }

    @Test
    fun `identity bytes cross the FFI unchanged`() {
        // A byte array marshalled wrong -- truncated, sign-extended, reordered
        // -- would still produce a plausible-looking offset. Only comparing it
        // to the core's own answer catches that.
        for (vector in coreFamilyRelayJitterVectors()) {
            assertEquals(
                vector.name,
                vector.expectedJitterMs,
                coreFamilyRelayJitterMs(vector.identityPublicBytes),
            )
        }
    }

    @Test
    fun `the shim composes the curve with the identity offset`() {
        // The one thing the shim adds beyond forwarding: it hands the core an
        // identity and gets back a window that already includes that
        // identity's offset. Pinned against the two pieces computed
        // separately, so the composition cannot silently drop the jitter.
        val identity = ByteArray(32) { it.toByte() }
        val backoff = FamilyRelayBackoff()
        val jitterMs = coreFamilyRelayJitterMs(identity)

        val first = backoff.onRateLimited(retryAfterMs = 1_000L, identityPublicBytes = identity)
        assertEquals(
            coreFamilyRelayBackoffDelayMs(1_000uL, 1u, jitterMs).toLong(),
            first,
        )
        assertEquals(1, backoff.consecutiveRateLimits)

        val second = backoff.onRateLimited(retryAfterMs = 1_000L, identityPublicBytes = identity)
        assertEquals(
            coreFamilyRelayBackoffDelayMs(1_000uL, 2u, jitterMs).toLong(),
            second,
        )
        assertEquals(2, backoff.consecutiveRateLimits)

        backoff.onSuccessfulPass()
        assertEquals(0, backoff.consecutiveRateLimits)
        assertEquals(
            first,
            backoff.onRateLimited(retryAfterMs = 1_000L, identityPublicBytes = identity),
        )
    }

    @Test
    fun `a negative retry-after cannot underflow the unsigned crossing`() {
        // Kotlin's Long meets the core's ULong here. A value that should never
        // occur must clamp rather than wrap into a multi-century quiet window.
        val backoff = FamilyRelayBackoff()
        val delayMs = backoff.onRateLimited(retryAfterMs = -5_000L, identityPublicBytes = ByteArray(0))
        assertEquals(
            coreFamilyRelayBackoffDelayMs(0uL, 1u, coreFamilyRelayJitterMs(ByteArray(0))).toLong(),
            delayMs,
        )
    }

    @Test
    fun `concurrent reservations never collide on one slot`() {
        // The engine paces from whichever thread is running the pass. Two
        // callers must get two slots, not the same one twice -- this is the
        // threading property the shim exists to keep, and it is not something
        // the core's own single-threaded tests can show.
        val pacer = FamilyRelayRequestPacer()
        val waits = java.util.Collections.synchronizedList(mutableListOf<Long>())
        val threads = (0 until 8).map {
            Thread { waits.add(pacer.reserve(0L)) }
        }
        threads.forEach { it.start() }
        threads.forEach { it.join() }

        assertEquals(8, waits.toSet().size)
        assertEquals(
            listOf(0L, 500L, 1_000L, 1_500L, 2_000L, 2_500L, 3_000L, 3_500L),
            waits.sorted(),
        )
    }
}
