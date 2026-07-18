package com.cruisemesh.app.mesh

import java.io.IOException
import java.net.ConnectException
import java.net.SocketException
import java.net.SocketTimeoutException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SweepOutcomesTest {
    @Test
    fun `completion summary is emitted exactly once with every outcome counted`() {
        val outcomes = SweepOutcomes(generation = 7, candidateCount = 5)
        assertNull(outcomes.record(6, SweepProbeOutcome.CONNECTED))

        assertNull(outcomes.record(7, SweepProbeOutcome.CONNECTED)?.completedSummary)
        assertNull(outcomes.record(7, SweepProbeOutcome.REFUSED)?.completedSummary)
        assertNull(outcomes.record(7, SweepProbeOutcome.TIMED_OUT)?.completedSummary)
        assertNull(outcomes.record(7, SweepProbeOutcome.DENIED)?.completedSummary)
        val summary = outcomes.record(7, SweepProbeOutcome.OTHER)?.completedSummary

        assertEquals(
            "Sweep complete (/16): 5 probed, 1 connected, 1 refused, 1 timed out, 1 denied, 1 other.",
            summary?.logLine(16),
        )
        assertNull(outcomes.record(7, SweepProbeOutcome.OTHER))
        assertEquals(0, outcomes.remainingCandidates())
    }

    @Test
    fun `probe failures are classified without Android dependencies`() {
        assertEquals(SweepProbeOutcome.REFUSED, classifySweepProbeFailure(ConnectException()))
        assertEquals(SweepProbeOutcome.TIMED_OUT, classifySweepProbeFailure(SocketTimeoutException()))
        assertEquals(SweepProbeOutcome.DENIED, classifySweepProbeFailure(SecurityException()))
        assertEquals(
            SweepProbeOutcome.DENIED,
            classifySweepProbeFailure(SocketException("socket failed: EPERM (Operation not permitted)")),
        )
        assertEquals(SweepProbeOutcome.OTHER, classifySweepProbeFailure(IOException("unusual failure")))
    }

    @Test
    fun `all silent broad sweep indicates isolation`() {
        assertEquals(
            LanSweepVerdict.ISOLATION_SUSPECTED,
            lanSweepVerdict(summary(timedOut = 253)),
        )
    }

    @Test
    fun `denied sweep indicates VPN or OS policy block without isolation`() {
        assertEquals(
            LanSweepVerdict.BLOCKED_BY_POLICY,
            lanSweepVerdict(summary(timedOut = 252, denied = 1)),
        )
    }

    @Test
    fun `refused host makes an empty sweep healthy rather than isolated`() {
        assertEquals(
            LanSweepVerdict.HEALTHY_BUT_EMPTY,
            lanSweepVerdict(summary(refused = 1, timedOut = 252)),
        )
    }

    @Test
    fun `a connected probe means the sweep found a peer`() {
        assertEquals(
            LanSweepVerdict.FOUND_PEER,
            lanSweepVerdict(summary(connected = 1, timedOut = 252)),
        )
    }

    private fun summary(
        connected: Int = 0,
        refused: Int = 0,
        timedOut: Int = 0,
        denied: Int = 0,
        other: Int = 0,
    ) = SweepOutcomeSummary(connected, refused, timedOut, denied, other)
}
