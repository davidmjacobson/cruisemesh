package com.cruisemesh.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The seven-tap run, as a plain counter with no Android in it.
 *
 * The rule this pins is the one a release tester depends on: seven deliberate
 * taps open the door, a stalled run does not, and nothing is said until the
 * fourth tap so an accidental double-tap stays invisible.
 */
class InternalToolsUnlockTest {

    @Test
    fun `seven taps in a row reach the threshold`() {
        val counter = InternalToolsTapCounter()
        val seen = (1..7).map { counter.tap(it * 100L) }

        assertEquals(
            listOf(
                InternalToolsTap.Quiet,
                InternalToolsTap.Quiet,
                InternalToolsTap.Quiet,
                InternalToolsTap.Countdown(3),
                InternalToolsTap.Countdown(2),
                InternalToolsTap.Countdown(1),
                InternalToolsTap.Reached,
            ),
            seen,
        )
    }

    @Test
    fun `reaching the threshold starts the next run from zero`() {
        val counter = InternalToolsTapCounter()
        repeat(7) { counter.tap(it * 100L) }

        // An eighth tap is the first of a fresh run, not an eighth of the old
        // one -- otherwise every further tap would toggle the flag again.
        assertEquals(InternalToolsTap.Quiet, counter.tap(800L))
    }

    @Test
    fun `a pause longer than the window starts the count over`() {
        val counter = InternalToolsTapCounter()
        repeat(6) { counter.tap(it * 100L) }

        assertEquals(InternalToolsTap.Quiet, counter.tap(500L + INTERNAL_TOOLS_TAP_WINDOW_MS + 1L))
    }

    @Test
    fun `taps at the edge of the window still continue the run`() {
        val counter = InternalToolsTapCounter()
        var now = 0L
        repeat(INTERNAL_TOOLS_UNLOCK_TAPS - 1) {
            counter.tap(now)
            now += INTERNAL_TOOLS_TAP_WINDOW_MS
        }

        assertEquals(InternalToolsTap.Reached, counter.tap(now))
    }

    @Test
    fun `a clock that jumps backwards starts the count over`() {
        val counter = InternalToolsTapCounter()
        repeat(6) { counter.tap(10_000L + it * 100L) }

        assertEquals(InternalToolsTap.Quiet, counter.tap(9_000L))
    }

    @Test
    fun `an explicit reset abandons a run in progress`() {
        val counter = InternalToolsTapCounter()
        repeat(6) { counter.tap(it * 100L) }
        counter.reset()

        assertEquals(InternalToolsTap.Quiet, counter.tap(700L))
    }
}
