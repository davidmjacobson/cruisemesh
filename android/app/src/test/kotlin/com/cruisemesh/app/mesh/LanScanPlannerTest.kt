package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class LanScanPlannerTest {
    private val minute = 60_000L

    @Test
    fun nothingIsDueBeforeJoiningOrAfterLosingTheNetwork() {
        val planner = LanScanPlanner()
        assertNull(planner.takeDueScan(0))

        planner.onNetworkJoined(1_000)
        planner.onNetworkLost()
        assertNull(planner.takeDueScan(2_000))
    }

    @Test
    fun joinRunsTheCheapLocalTierFirstAndEscalatesOnlyAfterItCompletes() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)

        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        // Full sweep is anchored at join but must wait for the local sweep.
        assertNull(planner.takeDueScan(1_000))

        planner.onScanCompleted(LanScanBreadth.LOCAL_24)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(2_000))
    }

    @Test
    fun localTierKeepsAFlatFiveMinuteCadence() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24)

        // Full sweep claimed; local not due again until five minutes in.
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(1_000))
        assertNull(planner.takeDueScan(4 * minute))
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(5 * minute))
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(10 * minute))
    }

    @Test
    fun fullSweepBacksOffFifteenMinutesThenAnHourThenCapsAtFourHours() {
        // A huge local interval isolates the full tier after its prerequisite.
        val planner = LanScanPlanner(localIntervalMs = Long.MAX_VALUE / 2)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24)

        var now = 1_000L
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(now))
        for (expectedGapMs in listOf(
            15 * minute,
            60 * minute,
            240 * minute,
            // The cap repeats.
            240 * minute,
        )) {
            assertNull(planner.takeDueScan(now + expectedGapMs - 1))
            now += expectedGapMs
            assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(now))
        }
    }

    @Test
    fun peerEvidenceMakesTheFullSweepDueAgainAndResetsItsBackoff() {
        val planner = LanScanPlanner(localIntervalMs = Long.MAX_VALUE / 2)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24)

        // Two claims advance the backoff to the one-hour step.
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(1_000))
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(1_000 + 15 * minute))
        assertNull(planner.takeDueScan(2_000 + 15 * minute))

        val evidenceAt = 3_000 + 15 * minute
        planner.onPeerEvidence(evidenceAt)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(evidenceAt))
        // And the next gap is back to the first backoff step, not an hour.
        assertEquals(
            LanScanBreadth.FULL_SUBNET,
            planner.takeDueScan(evidenceAt + 15 * minute),
        )
    }

    @Test
    fun rejoiningANetworkReanchorsBothTiers() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(1_000))

        // Off the ship and back on: everything starts over, local first.
        val rejoinAt = 26 * 60 * minute
        planner.onNetworkJoined(rejoinAt)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(rejoinAt))
        assertNull(planner.takeDueScan(rejoinAt + 1_000))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(rejoinAt + 2_000))
    }
}
