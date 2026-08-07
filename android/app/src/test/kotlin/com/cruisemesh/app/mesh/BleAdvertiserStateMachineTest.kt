package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The three silent-permanent-dark sequences from [BleAdvertiserStateMachine]'s
 * class doc, plus the ordinary lifecycle, as tests.
 *
 * "Dark" in every case below means the same observable thing: the machine ends
 * up believing it is advertising (so every later start request early-returns)
 * while no advertising set exists on the radio -- with nothing logged. Each
 * test therefore asserts on the *decision*, not on a boolean.
 */
class BleAdvertiserStateMachineTest {

    /** Drives a start to completion and returns the generation that is now live. */
    private fun BleAdvertiserStateMachine.startAndSucceed(): Long {
        val action = onStartRequested()
        val generation = requireNotNull(action.startGeneration)
        onStartSucceeded(generation)
        return generation
    }

    @Test
    fun `a start request registers a fresh generation and settles on success`() {
        val machine = BleAdvertiserStateMachine()
        assertEquals(AdvertiserState.IDLE, machine.state())

        val action = machine.onStartRequested()
        assertNull("nothing to stop on the first start", action.stopGeneration)
        val generation = requireNotNull(action.startGeneration)
        assertEquals(AdvertiserState.STARTING, machine.state())

        machine.onStartSucceeded(generation)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `redundant start requests never thrash the advertiser`() {
        val machine = BleAdvertiserStateMachine()
        val action = machine.onStartRequested()
        // Every link teardown calls beginAdvertising(); while a start is in
        // flight, and once it has succeeded, those must be no-ops.
        assertTrue(machine.onStartRequested().isNone)
        machine.onStartSucceeded(requireNotNull(action.startGeneration))
        assertTrue(machine.onStartRequested().isNone)
    }

    @Test
    fun `a connect restart stops exactly the generation it replaces`() {
        val machine = BleAdvertiserStateMachine()
        val live = machine.startAndSucceed()

        // Bug 1: this restart used to reach the framework with the same
        // already-registered callback and come back ADVERTISE_FAILED_ALREADY_STARTED,
        // which the old code read as success. The restart must name the old
        // generation for the stop and a brand-new one for the start.
        val restart = machine.onConnectRestartRequested()
        assertEquals(live, restart.stopGeneration)
        assertNotEquals(live, restart.startGeneration)
        assertEquals(AdvertiserState.STARTING, machine.state())
    }

    @Test
    fun `a retired generation's late disable cannot stop its successor`() {
        val machine = BleAdvertiserStateMachine()
        val old = machine.startAndSucceed()
        val restart = machine.onConnectRestartRequested()
        val new = requireNotNull(restart.startGeneration)
        machine.onStartSucceeded(new)

        // Bug 2: the framework keys its wrapper map on the callback OBJECT, so
        // reusing one callback let generation N's late disable stop generation
        // N+1. Generations are distinct here, and N's results are refused
        // outright, so nothing about N can touch N+1.
        assertNotEquals(old, new)
        assertFalse(machine.acceptsResultFor(old))
        assertTrue(machine.onStartSucceeded(old).isNone)
        assertTrue(machine.onStartFailed(old).isNone)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `a stale start success after stop cannot resurrect a dead advertiser`() {
        val machine = BleAdvertiserStateMachine()
        val startAction = machine.onStartRequested()
        val inFlight = requireNotNull(startAction.startGeneration)

        val stop = machine.onStopRequested()
        assertEquals(inFlight, stop.stopGeneration)
        assertEquals(AdvertiserState.IDLE, machine.state())

        // Bug 3: this callback used to land after stop() and set advertising
        // back to true, after which every restart early-returned forever.
        machine.onStartSucceeded(inFlight)
        assertEquals(AdvertiserState.IDLE, machine.state())

        // And the next start request must genuinely start something, under a
        // generation the stale callback can never be confused with.
        val restart = machine.onStartRequested()
        assertNotEquals(inFlight, requireNotNull(restart.startGeneration))
        assertEquals(AdvertiserState.STARTING, machine.state())
    }

    @Test
    fun `a duty mode change during an in-flight start is applied when it settles`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        val inFlight = requireNotNull(machine.onStartRequested().startGeneration)

        // The old code wrote currentAdvertiseMode and then early-returned on
        // `!advertising`, recording a mode it never applied.
        assertTrue(machine.onDutyModeRequested(RadioDutyMode.BALANCED).isNone)
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
        assertTrue(machine.hasRestartPending())

        val followUp = machine.onStartSucceeded(inFlight)
        assertEquals(inFlight, followUp.stopGeneration)
        assertNotEquals(inFlight, followUp.startGeneration)
        assertFalse(machine.hasRestartPending())
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `a duty mode change while advertising restarts immediately`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        val live = machine.startAndSucceed()

        val action = machine.onDutyModeRequested(RadioDutyMode.BALANCED)
        assertEquals(live, action.stopGeneration)
        assertNotEquals(live, action.startGeneration)
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `a duty mode change while idle only records the mode`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        // Not advertising: nothing to restart, and this must not start
        // advertising on its own (a policy tick is not a request to advertise).
        assertTrue(machine.onDutyModeRequested(RadioDutyMode.BALANCED).isNone)
        assertEquals(AdvertiserState.IDLE, machine.state())
        assertFalse(machine.hasRestartPending())
        // The next real start picks the new mode up.
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
        machine.onStartRequested()
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `an unchanged duty mode is a no-op`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        machine.startAndSucceed()
        // MeshService calls this on every policy tick.
        assertTrue(machine.onDutyModeRequested(RadioDutyMode.LOW_POWER).isNone)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `a start failure leaves the machine retryable`() {
        val machine = BleAdvertiserStateMachine()
        val generation = requireNotNull(machine.onStartRequested().startGeneration)
        machine.onStartFailed(generation)
        assertEquals(AdvertiserState.IDLE, machine.state())

        // ADVERTISE_FAILED_ALREADY_STARTED reaches this same path: the old code
        // mapped it to "advertising", which is what hid a dark radio. The next
        // trigger must be free to start a new generation.
        val retry = machine.onStartRequested()
        assertNotEquals(generation, retry.startGeneration)
        assertNull("a failed generation was never registered, so nothing to stop", retry.stopGeneration)
    }

    @Test
    fun `a restart queued behind a failing start is dropped rather than applied to nothing`() {
        val machine = BleAdvertiserStateMachine()
        val generation = requireNotNull(machine.onStartRequested().startGeneration)
        machine.onDutyModeRequested(RadioDutyMode.BALANCED)
        assertTrue(machine.hasRestartPending())

        machine.onStartFailed(generation)
        assertFalse(machine.hasRestartPending())
        assertEquals(AdvertiserState.IDLE, machine.state())
        // The mode itself survives -- only the restart it would have needed is gone.
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `stopping while idle asks the framework for nothing`() {
        val machine = BleAdvertiserStateMachine()
        assertTrue(machine.onStopRequested().isNone)
        assertEquals(AdvertiserState.IDLE, machine.state())
    }

    @Test
    fun `stopping an advertising generation retires it`() {
        val machine = BleAdvertiserStateMachine()
        val live = machine.startAndSucceed()
        val stop = machine.onStopRequested()
        assertEquals(live, stop.stopGeneration)
        assertNull(stop.startGeneration)
        assertFalse(machine.acceptsResultFor(live))
    }

    @Test
    fun `generations are never reused`() {
        val machine = BleAdvertiserStateMachine()
        val seen = mutableSetOf<Long>()
        repeat(5) {
            val generation = requireNotNull(machine.onStartRequested().startGeneration)
            assertTrue("generation $generation reused", seen.add(generation))
            machine.onStartSucceeded(generation)
            machine.onStopRequested()
        }
    }
}
