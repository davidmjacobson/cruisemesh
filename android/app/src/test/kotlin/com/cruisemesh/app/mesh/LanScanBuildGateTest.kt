package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LanScanBuildGateTest {
    @Test
    fun pendingCandidateBuildRejectsASecondScan() {
        val gate = LanScanBuildGate()
        val first = gate.tryReserve({ false }, { 1 })

        assertNotNull(first)
        assertNull(gate.tryReserve({ false }, { 2 }))
        gate.release(first!!)
        assertNotNull(gate.tryReserve({ false }, { 2 }))
    }

    @Test
    fun networkResetInvalidatesBuilderBeforeItCanPublish() {
        val gate = LanScanBuildGate()
        val token = gate.tryReserve({ false }, { 1 })!!
        var published = false

        gate.reset { published = false }

        assertFalse(
            gate.activate(token) {
                published = true
                true
            },
        )
        assertFalse(published)
        assertNotNull(gate.tryReserve({ false }, { 2 }))
    }

    @Test
    fun activationPublishesOnceAndReleasesPendingAdmission() {
        val gate = LanScanBuildGate()
        val token = gate.tryReserve({ false }, { 1 })!!
        var publications = 0

        assertTrue(
            gate.activate(token) {
                publications += 1
                true
            },
        )
        assertFalse(gate.activate(token) { true })
        assertTrue(publications == 1)
        assertNotNull(gate.tryReserve({ false }, { 2 }))
    }

    @Test
    fun staleCompletionCannotClearAReplacementSweep() {
        val gate = LanScanBuildGate()
        var currentSweep = "old"
        val token = gate.tryReserve({ false }, { 2 })!!

        assertTrue(
            gate.activate(token) {
                currentSweep = "new"
                true
            },
        )
        assertFalse(
            gate.finishSweep(
                isCurrent = { currentSweep == "old" },
                clear = { currentSweep = "cleared" },
            ),
        )
        assertTrue(currentSweep == "new")
    }
}
