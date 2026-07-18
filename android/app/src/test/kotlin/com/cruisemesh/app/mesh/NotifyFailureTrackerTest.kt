package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NotifyFailureTrackerTest {

    @Test
    fun `a single failure does not trip teardown below the threshold`() {
        val tracker = NotifyFailureTracker(maxConsecutiveFailures = 3)
        assertFalse(tracker.recordFailure("AA:BB"))
        assertEquals(1, tracker.failureCount("AA:BB"))
    }

    @Test
    fun `teardown fires exactly on the Nth consecutive failure`() {
        val tracker = NotifyFailureTracker(maxConsecutiveFailures = 3)
        assertFalse(tracker.recordFailure("AA:BB"))
        assertFalse(tracker.recordFailure("AA:BB"))
        assertTrue(tracker.recordFailure("AA:BB"))
    }

    @Test
    fun `a success resets the streak so a later failure starts counting from zero`() {
        val tracker = NotifyFailureTracker(maxConsecutiveFailures = 3)
        tracker.recordFailure("AA:BB")
        tracker.recordFailure("AA:BB")
        tracker.recordSuccess("AA:BB")
        assertEquals(0, tracker.failureCount("AA:BB"))
        assertFalse(tracker.recordFailure("AA:BB"))
        assertFalse(tracker.recordFailure("AA:BB"))
        assertTrue(tracker.recordFailure("AA:BB"))
    }

    @Test
    fun `clear forgets an address's failure history`() {
        val tracker = NotifyFailureTracker(maxConsecutiveFailures = 2)
        tracker.recordFailure("AA:BB")
        tracker.clear("AA:BB")
        assertEquals(0, tracker.failureCount("AA:BB"))
        assertFalse(tracker.recordFailure("AA:BB"))
    }

    @Test
    fun `clearAll forgets every address`() {
        val tracker = NotifyFailureTracker(maxConsecutiveFailures = 2)
        tracker.recordFailure("AA:BB")
        tracker.recordFailure("CC:DD")
        tracker.clearAll()
        assertEquals(0, tracker.failureCount("AA:BB"))
        assertEquals(0, tracker.failureCount("CC:DD"))
    }

    @Test
    fun `failures on one address never affect a different address`() {
        val tracker = NotifyFailureTracker(maxConsecutiveFailures = 2)
        tracker.recordFailure("AA:BB")
        tracker.recordFailure("AA:BB")
        assertTrue(tracker.failureCount("AA:BB") >= 2)
        assertEquals(0, tracker.failureCount("CC:DD"))
        assertFalse(tracker.recordFailure("CC:DD"))
    }

    @Test
    fun `the field-log burst of 14 consecutive failures only tears down once the threshold is crossed`() {
        // Regression coverage for the exact shape of the 2026-07-17 log: 14
        // status=129 failures in a row for one address. With the default
        // threshold (3), teardown should trip on the 3rd and every failure
        // after that should also report "already past threshold" (true) --
        // the caller stops calling recordFailure once it tears down for
        // real, but the tracker itself must not flip back to false.
        val tracker = NotifyFailureTracker()
        val results = (1..14).map { tracker.recordFailure("6C:1A:F4:F3:33:3A") }
        assertEquals(false, results[0])
        assertEquals(false, results[1])
        assertTrue(results.drop(2).all { it })
    }
}
