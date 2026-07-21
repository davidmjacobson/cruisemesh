package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class LanScanPlannerTest {
    private val minute = 60_000L
    private val emptyDelay = LanScanPlanner.EMPTY_LOCAL_SWEEP_FULL_DELAY_MS

    @Test
    fun nothingIsDueBeforeJoiningOrAfterLosingTheNetwork() {
        val planner = LanScanPlanner()
        assertNull(planner.takeDueScan(0))

        planner.onNetworkJoined(1_000)
        planner.onNetworkLost()
        assertNull(planner.takeDueScan(2_000))
    }

    @Test
    fun joinRunsTheCheapLocalTierFirstAndTheFullSweepIsNotDueUntilAnEmptyLocalSweepArmsIt() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)

        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        // Not due at network-join anymore -- only after an empty /24 sweep.
        assertNull(planner.takeDueScan(1_000))

        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 1_000, foundPeer = false)
        // Armed, but not immediately: a real delay applies.
        assertNull(planner.takeDueScan(1_000 + emptyDelay - 1))
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(1_000 + emptyDelay))
    }

    @Test
    fun localSweepThatFindsAPeerNeverArmsTheFullSweep() {
        val planner = LanScanPlanner(localIntervalMs = Long.MAX_VALUE / 2)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))

        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 1_000, foundPeer = true)
        // A /24 sweep that found a peer must not arm the full tier at all.
        assertNull(planner.takeDueScan(1_000 + emptyDelay))
        assertNull(planner.takeDueScan(10 * 60 * minute))
    }

    @Test
    fun onceArmedALaterNonEmptyLocalSweepDoesNotDisarmOrRescheduleTheFullSweep() {
        val planner = LanScanPlanner(localIntervalMs = 5 * minute)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)

        // A later local sweep (still before the full sweep fires) that
        // *does* find a peer must not push the already-armed schedule out.
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(5 * minute))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 5 * minute, foundPeer = true)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(emptyDelay))
    }

    @Test
    fun localTierKeepsAFlatFiveMinuteCadence() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)

        // Full sweep claimed; local not due again until five minutes in.
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(emptyDelay))
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
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)

        var now = emptyDelay
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
    fun peerEvidenceIsANoOpBeforeTheFullSweepIsEligible() {
        val planner = LanScanPlanner(localIntervalMs = Long.MAX_VALUE / 2)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        // No completed /24 sweep yet, so nothing is armed; evidence must not
        // conjure a full sweep out of nowhere.
        planner.onPeerEvidence(500)
        assertNull(planner.takeDueScan(500))
    }

    @Test
    fun peerEvidenceMakesTheFullSweepDueAgainAndResetsItsBackoff() {
        val planner = LanScanPlanner(localIntervalMs = Long.MAX_VALUE / 2)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)

        // Two claims advance the backoff to the one-hour step.
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(emptyDelay))
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(emptyDelay + 15 * minute))
        assertNull(planner.takeDueScan(emptyDelay + 2_000 + 15 * minute))

        val evidenceAt = emptyDelay + 3_000 + 15 * minute
        planner.onPeerEvidence(evidenceAt)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(evidenceAt))
        // And the next gap is back to the first backoff step, not an hour.
        assertEquals(
            LanScanBreadth.FULL_SUBNET,
            planner.takeDueScan(evidenceAt + 15 * minute),
        )
    }

    @Test
    fun rejoiningANetworkReanchorsBothTiersAndDisarmsTheFullTier() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(emptyDelay))

        // Off the ship and back on: everything starts over, local first.
        val rejoinAt = 26 * 60 * minute
        planner.onNetworkJoined(rejoinAt)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(rejoinAt))
        // Disarmed on rejoin: not due even once the old empty-sweep delay
        // would have elapsed, and not due until a fresh /24 sweep completes.
        assertNull(planner.takeDueScan(rejoinAt + emptyDelay))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, rejoinAt + emptyDelay, foundPeer = false)
        assertEquals(
            LanScanBreadth.FULL_SUBNET,
            planner.takeDueScan(rejoinAt + emptyDelay + emptyDelay),
        )
    }

    @Test
    fun isolationDefersTheFullSweepToTheCapUntilPeerEvidenceResetsIt() {
        val planner = LanScanPlanner(localIntervalMs = Long.MAX_VALUE / 2)
        planner.onNetworkJoined(0)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(0))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)

        val isolationAt = 10_000L
        planner.onIsolationSuspected(isolationAt)
        assertNull(planner.takeDueScan(isolationAt + 4 * 60 * minute - 1))
        assertEquals(
            LanScanBreadth.FULL_SUBNET,
            planner.takeDueScan(isolationAt + 4 * 60 * minute),
        )

        val evidenceAt = isolationAt + 4 * 60 * minute + 1_000
        planner.onIsolationSuspected(evidenceAt)
        planner.onPeerEvidence(evidenceAt + 1_000)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(evidenceAt + 1_000))
    }

    @Test
    fun networkJoinResetsAnIsolationDeferral() {
        val planner = LanScanPlanner()
        planner.onNetworkJoined(0)
        planner.takeDueScan(0)
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 0, foundPeer = false)
        planner.onIsolationSuspected(1_000)

        planner.onNetworkJoined(2_000)
        assertEquals(LanScanBreadth.LOCAL_24, planner.takeDueScan(2_000))
        planner.onScanCompleted(LanScanBreadth.LOCAL_24, 2_000, foundPeer = false)
        assertEquals(LanScanBreadth.FULL_SUBNET, planner.takeDueScan(2_000 + emptyDelay))
    }
}
