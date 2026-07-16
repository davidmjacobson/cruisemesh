package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class LanHealthTrackerTest {
    @Test
    fun responseReportsLatencyAndResetsFailureState() {
        val tracker = LanHealthTracker(timeoutMs = 100, maxConsecutiveTimeouts = 3)
        assertEquals(
            LanHealthTracker.Decision.Send(1u),
            tracker.next("lan:1", nowMs = 1_000, nonce = 1u),
        )
        assertEquals(25L, tracker.response("lan:1", nonce = 1u, nowMs = 1_025))
        assertEquals(
            LanHealthTracker.Decision.Send(2u),
            tracker.next("lan:1", nowMs = 2_000, nonce = 2u),
        )
    }

    @Test
    fun staleLinkClosesAfterBoundedConsecutiveTimeouts() {
        val tracker = LanHealthTracker(timeoutMs = 100, maxConsecutiveTimeouts = 2)
        assertEquals(
            LanHealthTracker.Decision.Send(1u),
            tracker.next("lan:1", nowMs = 0, nonce = 1u),
        )
        assertEquals(
            LanHealthTracker.Decision.Send(2u),
            tracker.next("lan:1", nowMs = 100, nonce = 2u),
        )
        assertEquals(
            LanHealthTracker.Decision.Close,
            tracker.next("lan:1", nowMs = 200, nonce = 3u),
        )
        assertNull(tracker.response("lan:1", nonce = 2u, nowMs = 201))
    }
}
