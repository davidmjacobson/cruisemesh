package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The three silent-permanent-dark sequences from [BleAdvertiserStateMachine]'s
 * class doc, the two stuck-state sequences the watchdog exists for, plus the
 * ordinary lifecycle.
 *
 * "Dark" in every case below means the same observable thing: the machine ends
 * up believing it is advertising or starting (so every later start request
 * early-returns) while no advertising set exists on the radio -- with nothing
 * logged. Each test therefore asserts on the *decision*, not on a boolean.
 */
class BleAdvertiserStateMachineTest {

    /** All times are the monotonic milliseconds the machine expects. */
    private var now = 10_000L

    private fun BleAdvertiserStateMachine.start() = onStartRequested(now)

    /** Drives a start to completion and returns the generation that is now live. */
    private fun BleAdvertiserStateMachine.startAndSucceed(): Long {
        val action = start()
        val generation = requireNotNull(action.startGeneration)
        onStartSucceeded(generation, now)
        return generation
    }

    @Test
    fun `a start request registers a fresh generation and settles on success`() {
        val machine = BleAdvertiserStateMachine()
        assertEquals(AdvertiserState.IDLE, machine.state())

        val action = machine.start()
        assertNull("nothing to stop on the first start", action.stopGeneration)
        val generation = requireNotNull(action.startGeneration)
        assertEquals(AdvertiserState.STARTING, machine.state())
        assertEquals(
            "every start is watched, including the first",
            BleAdvertiserStateMachine.START_WATCHDOG_MS,
            action.watchdogInMs,
        )

        machine.onStartSucceeded(generation, now)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `redundant start requests never thrash the advertiser`() {
        val machine = BleAdvertiserStateMachine()
        val action = machine.start()
        // Every link teardown calls beginAdvertising(); while a start is in
        // flight, and once it has succeeded, those must be no-ops.
        assertTrue(machine.start().isNone)
        machine.onStartSucceeded(requireNotNull(action.startGeneration), now)
        assertTrue(machine.start().isNone)
    }

    @Test
    fun `a connect restart stops exactly the generation it replaces`() {
        val machine = BleAdvertiserStateMachine()
        val live = machine.startAndSucceed()

        // Bug 1: this restart used to reach the framework with the same
        // already-registered callback and come back ADVERTISE_FAILED_ALREADY_STARTED,
        // which the old code read as success. The restart must name the old
        // generation for the stop and a brand-new one for the start.
        val restart = machine.onConnectRestartRequested(now)
        assertEquals(live, restart.stopGeneration)
        assertNotEquals(live, restart.startGeneration)
        assertEquals(AdvertiserState.STARTING, machine.state())
    }

    @Test
    fun `a retired generation's late disable cannot stop its successor`() {
        val machine = BleAdvertiserStateMachine()
        val old = machine.startAndSucceed()
        val restart = machine.onConnectRestartRequested(now)
        val new = requireNotNull(restart.startGeneration)
        machine.onStartSucceeded(new, now)

        // Bug 2: the framework keys its wrapper map on the callback OBJECT, so
        // reusing one callback let generation N's late disable stop generation
        // N+1. Generations are distinct here, and N's results are refused
        // outright, so nothing about N can touch N+1.
        assertNotEquals(old, new)
        assertFalse(machine.acceptsResultFor(old))
        assertTrue(machine.onStartSucceeded(old, now).isNone)
        assertTrue(machine.onStartFailed(old, now).isNone)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `a stale start success after stop cannot resurrect a dead advertiser`() {
        val machine = BleAdvertiserStateMachine()
        val startAction = machine.start()
        val inFlight = requireNotNull(startAction.startGeneration)

        val stop = machine.onStopRequested()
        assertEquals(inFlight, stop.stopGeneration)
        assertEquals(AdvertiserState.IDLE, machine.state())

        // Bug 3: this callback used to land after stop() and set advertising
        // back to true, after which every restart early-returned forever.
        machine.onStartSucceeded(inFlight, now)
        assertEquals(AdvertiserState.IDLE, machine.state())

        // And the next start request must genuinely start something, under a
        // generation the stale callback can never be confused with.
        val restart = machine.start()
        assertNotEquals(inFlight, requireNotNull(restart.startGeneration))
        assertEquals(AdvertiserState.STARTING, machine.state())
    }

    @Test
    fun `a duty mode change during an in-flight start is applied when it settles`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        val inFlight = requireNotNull(machine.start().startGeneration)

        // The old code wrote currentAdvertiseMode and then early-returned on
        // `!advertising`, recording a mode it never applied.
        assertTrue(machine.onDutyModeRequested(RadioDutyMode.BALANCED, now).isNone)
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
        assertTrue(machine.hasRestartPending())

        val followUp = machine.onStartSucceeded(inFlight, now)
        assertEquals(inFlight, followUp.stopGeneration)
        assertNotEquals(inFlight, followUp.startGeneration)
        assertFalse(machine.hasRestartPending())
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `a duty mode change while advertising restarts immediately`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        val live = machine.startAndSucceed()

        val action = machine.onDutyModeRequested(RadioDutyMode.BALANCED, now)
        assertEquals(live, action.stopGeneration)
        assertNotEquals(live, action.startGeneration)
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `a duty mode change while idle only records the mode`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        // Not advertising: nothing to restart, and this must not start
        // advertising on its own (a policy tick is not a request to advertise).
        assertTrue(machine.onDutyModeRequested(RadioDutyMode.BALANCED, now).isNone)
        assertEquals(AdvertiserState.IDLE, machine.state())
        assertFalse(machine.hasRestartPending())
        // The next real start picks the new mode up.
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
        machine.start()
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
    }

    @Test
    fun `an unchanged duty mode is a no-op`() {
        val machine = BleAdvertiserStateMachine(initialMode = RadioDutyMode.LOW_POWER)
        machine.startAndSucceed()
        // MeshService calls this on every policy tick.
        assertTrue(machine.onDutyModeRequested(RadioDutyMode.LOW_POWER, now).isNone)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `a failed start re-arms itself instead of waiting for a link event`() {
        val machine = BleAdvertiserStateMachine()
        val generation = requireNotNull(machine.start().startGeneration)

        // The failure that motivates this: a connect-triggered restart comes
        // back ADVERTISE_FAILED_INTERNAL_ERROR / TOO_MANY_ADVERTISERS. The only
        // organic re-triggers are link events, and a phone that is not
        // advertising stops getting them -- so parking in IDLE with nothing
        // scheduled is a permanent dark radio.
        val failure = machine.onStartFailed(generation, now)
        assertEquals(AdvertiserState.IDLE, machine.state())
        assertTrue(machine.hasRetryPending())
        assertEquals(BleAdvertiserStateMachine.RETRY_MIN_DELAY_MS, failure.watchdogInMs)

        // Early ticks do not retry, they just re-arm for the remaining time.
        now += BleAdvertiserStateMachine.RETRY_MIN_DELAY_MS / 2
        val early = machine.onWatchdogDue(now)
        assertTrue(early.isNone)
        assertNotNull(early.watchdogInMs)
        assertEquals(AdvertiserState.IDLE, machine.state())

        now += BleAdvertiserStateMachine.RETRY_MIN_DELAY_MS
        val retry = machine.onWatchdogDue(now)
        assertNotEquals(generation, requireNotNull(retry.startGeneration))
        assertNull("a failed generation was never registered, so nothing to stop", retry.stopGeneration)
        assertEquals(AdvertiserState.STARTING, machine.state())
        assertFalse(machine.hasRetryPending())
    }

    @Test
    fun `repeated start failures back off but never give up`() {
        val machine = BleAdvertiserStateMachine()
        var generation = requireNotNull(machine.start().startGeneration)
        val delays = mutableListOf<Long>()
        repeat(12) {
            val delayMs = requireNotNull(machine.onStartFailed(generation, now).watchdogInMs)
            delays += delayMs
            now += delayMs
            generation = requireNotNull(machine.onWatchdogDue(now).startGeneration)
        }

        assertEquals(BleAdvertiserStateMachine.RETRY_MIN_DELAY_MS, delays.first())
        assertTrue("the delay must grow while failures keep coming", delays[1] > delays[0])
        delays.zipWithNext { earlier, later ->
            assertTrue("backoff must never shrink mid-streak", later >= earlier)
        }
        // Capped, not unbounded: a phone whose adapter is refusing
        // advertisements keeps trying at a sane rate rather than either
        // hammering the radio or ever giving up on being discoverable.
        assertEquals(BleAdvertiserStateMachine.RETRY_MAX_DELAY_MS, delays.last())
        assertTrue(delays.all { it <= BleAdvertiserStateMachine.RETRY_MAX_DELAY_MS })
    }

    @Test
    fun `a start the framework never answers is force-retired rather than absorbed`() {
        val machine = BleAdvertiserStateMachine()
        val stuck = requireNotNull(machine.start().startGeneration)

        // AOSP can swallow a startAdvertising failure without ever calling back.
        // Without the watchdog, STARTING absorbs every later request forever --
        // including a connect restart, which only sets restartPending -- so the
        // phone stays dark until Bluetooth is toggled.
        assertTrue(machine.start().isNone)
        assertTrue(machine.onConnectRestartRequested(now).isNone)

        // A tick before the deadline just re-arms.
        now += BleAdvertiserStateMachine.START_WATCHDOG_MS / 2
        val early = machine.onWatchdogDue(now)
        assertTrue(early.isNone)
        assertEquals(AdvertiserState.STARTING, machine.state())

        now += BleAdvertiserStateMachine.START_WATCHDOG_MS
        val expired = machine.onWatchdogDue(now)
        assertEquals("the unanswered generation must be unregistered", stuck, expired.stopGeneration)
        assertNull("and retried on the backoff, not instantly", expired.startGeneration)
        assertEquals(AdvertiserState.IDLE, machine.state())
        assertTrue(machine.hasRetryPending())

        now += requireNotNull(expired.watchdogInMs)
        val retry = machine.onWatchdogDue(now)
        assertNotEquals(stuck, requireNotNull(retry.startGeneration))
        assertEquals(AdvertiserState.STARTING, machine.state())
    }

    @Test
    fun `a very late callback from a force-retired generation is refused`() {
        val machine = BleAdvertiserStateMachine()
        val stuck = requireNotNull(machine.start().startGeneration)
        now += BleAdvertiserStateMachine.START_WATCHDOG_MS
        val expired = machine.onWatchdogDue(now)
        now += requireNotNull(expired.watchdogInMs)
        val live = requireNotNull(machine.onWatchdogDue(now).startGeneration)

        assertFalse(machine.acceptsResultFor(stuck))
        assertTrue(machine.onStartSucceeded(stuck, now).isNone)
        assertEquals(AdvertiserState.STARTING, machine.state())
        machine.onStartSucceeded(live, now)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `a successful start clears the failure backoff`() {
        val machine = BleAdvertiserStateMachine()
        val failed = requireNotNull(machine.start().startGeneration)
        machine.onStartFailed(failed, now)
        now += BleAdvertiserStateMachine.RETRY_MIN_DELAY_MS
        val retry = requireNotNull(machine.onWatchdogDue(now).startGeneration)
        machine.onStartSucceeded(retry, now)
        assertFalse(machine.hasRetryPending())

        // The next failure starts from the minimum again rather than from
        // wherever the previous streak left off.
        val next = requireNotNull(machine.onConnectRestartRequested(now).startGeneration)
        assertEquals(
            BleAdvertiserStateMachine.RETRY_MIN_DELAY_MS,
            machine.onStartFailed(next, now).watchdogInMs,
        )
    }

    @Test
    fun `a start request beats a pending retry to it`() {
        val machine = BleAdvertiserStateMachine()
        val failed = requireNotNull(machine.start().startGeneration)
        machine.onStartFailed(failed, now)
        assertTrue(machine.hasRetryPending())

        // A link teardown is a fresher reason to advertise than the timer.
        val started = machine.onStartRequested(now)
        assertNotNull(started.startGeneration)
        assertFalse(machine.hasRetryPending())
        // ...and the stale tick that follows must not start a second generation.
        assertTrue(machine.onWatchdogDue(now).isNone)
    }

    @Test
    fun `a restart queued behind a failing start is dropped rather than applied to nothing`() {
        val machine = BleAdvertiserStateMachine()
        val generation = requireNotNull(machine.start().startGeneration)
        machine.onDutyModeRequested(RadioDutyMode.BALANCED, now)
        assertTrue(machine.hasRestartPending())

        machine.onStartFailed(generation, now)
        assertFalse(machine.hasRestartPending())
        assertEquals(AdvertiserState.IDLE, machine.state())
        // The mode itself survives -- only the restart it would have needed is
        // gone, and the re-arm will start a generation carrying it.
        assertEquals(RadioDutyMode.BALANCED, machine.desiredMode())
        assertTrue(machine.hasRetryPending())
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
    fun `stopping drops a pending retry so a stopped peripheral stays stopped`() {
        val machine = BleAdvertiserStateMachine()
        val generation = requireNotNull(machine.start().startGeneration)
        machine.onStartFailed(generation, now)
        assertTrue(machine.hasRetryPending())

        machine.onStopRequested()
        assertFalse(machine.hasRetryPending())
        // A leftover tick must not bring advertising back up behind stop()'s back.
        now += BleAdvertiserStateMachine.RETRY_MAX_DELAY_MS
        assertTrue(machine.onWatchdogDue(now).isNone)
        assertEquals(AdvertiserState.IDLE, machine.state())
    }

    @Test
    fun `a watchdog tick while advertising is free`() {
        val machine = BleAdvertiserStateMachine()
        machine.startAndSucceed()
        now += BleAdvertiserStateMachine.START_WATCHDOG_MS * 10
        val tick = machine.onWatchdogDue(now)
        assertTrue(tick.isNone)
        assertNull(tick.watchdogInMs)
        assertEquals(AdvertiserState.ADVERTISING, machine.state())
    }

    @Test
    fun `generations are never reused`() {
        val machine = BleAdvertiserStateMachine()
        val seen = mutableSetOf<Long>()
        repeat(5) {
            val generation = requireNotNull(machine.start().startGeneration)
            assertTrue("generation $generation reused", seen.add(generation))
            machine.onStartSucceeded(generation, now)
            machine.onStopRequested()
        }
    }
}
