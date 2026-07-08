package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ReconnectBackoffTrackerTest {

    @Test
    fun `a never-seen address is immediately eligible`() {
        val tracker = ReconnectBackoffTracker()
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 0L))
    }

    @Test
    fun `a failure blocks retry until the backoff elapses`() {
        val tracker = ReconnectBackoffTracker(initialBackoffMs = 1_000L)
        tracker.recordFailure("AA:BB", nowMs = 0L)
        assertFalse(tracker.canAttempt("AA:BB", nowMs = 500L))
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 1_000L))
    }

    @Test
    fun `backoff doubles on each consecutive failure up to the cap`() {
        val tracker = ReconnectBackoffTracker(
            initialBackoffMs = 1_000L,
            maxBackoffMs = 5_000L,
            maxConsecutiveFailures = 100,
        )
        tracker.recordFailure("AA:BB", nowMs = 0L) // next eligible at 1_000
        assertFalse(tracker.canAttempt("AA:BB", nowMs = 999L))
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 1_000L))

        tracker.recordFailure("AA:BB", nowMs = 1_000L) // next eligible at 1_000 + 2_000
        assertFalse(tracker.canAttempt("AA:BB", nowMs = 2_999L))
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 3_000L))

        tracker.recordFailure("AA:BB", nowMs = 3_000L) // next eligible at 3_000 + 4_000
        assertFalse(tracker.canAttempt("AA:BB", nowMs = 6_999L))
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 7_000L))

        tracker.recordFailure("AA:BB", nowMs = 7_000L) // would be 8_000 but capped at maxBackoffMs=5_000
        assertFalse(tracker.canAttempt("AA:BB", nowMs = 11_999L))
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 12_000L))
    }

    @Test
    fun `an address is given up after the consecutive-failure budget is exceeded`() {
        val tracker = ReconnectBackoffTracker(
            initialBackoffMs = 1L,
            maxBackoffMs = 1L,
            maxConsecutiveFailures = 3,
        )
        assertFalse(tracker.isGivenUp("AA:BB"))
        repeat(3) { tracker.recordFailure("AA:BB", nowMs = it.toLong()) }
        assertTrue(tracker.isGivenUp("AA:BB"))
        // Even long after the last backoff window, a given-up address stays refused.
        assertFalse(tracker.canAttempt("AA:BB", nowMs = Long.MAX_VALUE / 2))
    }

    @Test
    fun `recordSuccess clears failure history so the address is immediately eligible again`() {
        val tracker = ReconnectBackoffTracker(
            initialBackoffMs = 1_000L,
            maxConsecutiveFailures = 2,
        )
        tracker.recordFailure("AA:BB", nowMs = 0L)
        tracker.recordFailure("AA:BB", nowMs = 0L)
        assertTrue(tracker.isGivenUp("AA:BB"))

        tracker.recordSuccess("AA:BB")

        assertFalse(tracker.isGivenUp("AA:BB"))
        assertEquals(0, tracker.failureCount("AA:BB"))
        assertTrue(tracker.canAttempt("AA:BB", nowMs = 0L))
    }

    @Test
    fun `giving up on one address never blocks a different address`() {
        val tracker = ReconnectBackoffTracker(
            initialBackoffMs = 1_000L,
            maxConsecutiveFailures = 2,
        )
        repeat(2) { tracker.recordFailure("STALE", nowMs = 0L) }
        assertTrue(tracker.isGivenUp("STALE"))

        // A peer rotating to a new address (or a different peer entirely) is a
        // fresh map key with no history -- rediscovery is never blocked by an
        // unrelated address's failure count.
        assertTrue(tracker.canAttempt("FRESH", nowMs = 0L))
        assertFalse(tracker.isGivenUp("FRESH"))
    }
}
