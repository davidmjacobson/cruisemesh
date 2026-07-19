package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class LanSweepDisplayTrackerTest {
    @Test
    fun `isolation appears only after a completed broad sweep`() {
        val tracker = LanSweepDisplayTracker()

        assertEquals(LanSweepDisplayState.NONE, tracker.current())
        assertEquals(LanSweepDisplayState.CHECKING, tracker.onNetworkJoined())
        assertEquals(LanSweepDisplayState.CHECKING, tracker.onSweepStarted())
        assertEquals(
            LanSweepDisplayState.NONE,
            tracker.onSweepCompleted(summary(timedOut = 252)),
        )

        tracker.onSweepStarted()
        assertEquals(
            LanSweepDisplayState.ISOLATION_SUSPECTED,
            tracker.onSweepCompleted(summary(timedOut = 253)),
        )
    }

    @Test
    fun `network change replaces a stale verdict with checking`() {
        val tracker = LanSweepDisplayTracker()
        tracker.onSweepCompleted(summary(timedOut = 253))

        assertEquals(LanSweepDisplayState.CHECKING, tracker.onNetworkJoined())
    }

    @Test
    fun `peer evidence clears a completed isolation verdict`() {
        val tracker = LanSweepDisplayTracker()
        tracker.onSweepCompleted(summary(timedOut = 253))

        assertEquals(LanSweepDisplayState.NONE, tracker.onPeerEvidence())
    }

    @Test
    fun `late sweep completion cannot resurrect verdict after peer evidence`() {
        val tracker = LanSweepDisplayTracker()
        tracker.onNetworkJoined()
        tracker.onSweepStarted()
        tracker.onPeerEvidence()

        assertEquals(
            LanSweepDisplayState.NONE,
            tracker.onSweepCompleted(summary(timedOut = 253)),
        )
    }

    @Test
    fun `losing Wi-Fi clears every sweep display state`() {
        val tracker = LanSweepDisplayTracker()
        tracker.onNetworkJoined()

        assertEquals(LanSweepDisplayState.NONE, tracker.onNetworkLost())
    }

    private fun summary(
        connected: Int = 0,
        refused: Int = 0,
        timedOut: Int = 0,
        denied: Int = 0,
        other: Int = 0,
    ) = SweepOutcomeSummary(connected, refused, timedOut, denied, other)
}
