package com.cruisemesh.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The seven-tap run, as a plain counter with no Android in it.
 *
 * The rule this pins is the one a release tester depends on: seven deliberate
 * taps open the door, a stalled run does not, and nothing is said until the
 * fourth tap so an accidental double-tap stays invisible.
 */
class DeveloperSettingsUnlockTest {

    @Test
    fun `seven taps in a row reach the threshold`() {
        val counter = DeveloperSettingsTapCounter()
        val seen = (1..7).map { counter.tap(it * 100L) }

        assertEquals(
            listOf(
                DeveloperSettingsTap.Quiet,
                DeveloperSettingsTap.Quiet,
                DeveloperSettingsTap.Quiet,
                DeveloperSettingsTap.Countdown(3),
                DeveloperSettingsTap.Countdown(2),
                DeveloperSettingsTap.Countdown(1),
                DeveloperSettingsTap.Reached,
            ),
            seen,
        )
    }

    @Test
    fun `reaching the threshold starts the next run from zero`() {
        val counter = DeveloperSettingsTapCounter()
        repeat(7) { counter.tap(it * 100L) }

        // An eighth tap is the first of a fresh run, not an eighth of the old
        // one -- otherwise every further tap would toggle the flag again.
        assertEquals(DeveloperSettingsTap.Quiet, counter.tap(800L))
    }

    @Test
    fun `a pause longer than the window starts the count over`() {
        val counter = DeveloperSettingsTapCounter()
        repeat(6) { counter.tap(it * 100L) }

        assertEquals(DeveloperSettingsTap.Quiet, counter.tap(500L + DEVELOPER_SETTINGS_TAP_WINDOW_MS + 1L))
    }

    @Test
    fun `taps at the edge of the window still continue the run`() {
        val counter = DeveloperSettingsTapCounter()
        var now = 0L
        repeat(DEVELOPER_SETTINGS_UNLOCK_TAPS - 1) {
            counter.tap(now)
            now += DEVELOPER_SETTINGS_TAP_WINDOW_MS
        }

        assertEquals(DeveloperSettingsTap.Reached, counter.tap(now))
    }

    @Test
    fun `a clock that jumps backwards starts the count over`() {
        val counter = DeveloperSettingsTapCounter()
        repeat(6) { counter.tap(10_000L + it * 100L) }

        assertEquals(DeveloperSettingsTap.Quiet, counter.tap(9_000L))
    }

    @Test
    fun `an explicit reset abandons a run in progress`() {
        val counter = DeveloperSettingsTapCounter()
        repeat(6) { counter.tap(it * 100L) }
        counter.reset()

        assertEquals(DeveloperSettingsTap.Quiet, counter.tap(700L))
    }

    @Test
    fun `the version row says what each tap did`() {
        assertEquals(
            DeveloperSettingsLabel.Version,
            developerSettingsLabelFor(DeveloperSettingsTap.Quiet, unlockedAfterTap = false),
        )
        assertEquals(
            DeveloperSettingsLabel.Countdown(3),
            developerSettingsLabelFor(DeveloperSettingsTap.Countdown(3), unlockedAfterTap = false),
        )
        assertEquals(
            DeveloperSettingsLabel.Unlocked,
            developerSettingsLabelFor(DeveloperSettingsTap.Reached, unlockedAfterTap = true),
        )
        assertEquals(
            DeveloperSettingsLabel.Hidden,
            developerSettingsLabelFor(DeveloperSettingsTap.Reached, unlockedAfterTap = false),
        )
    }

    @Test
    fun `an early tap clears feedback left over from an abandoned run`() {
        // Taps one to three of a fresh run put the version string back, so a
        // count from a run that was given up on cannot linger under a new one.
        assertEquals(
            DeveloperSettingsLabel.Version,
            developerSettingsLabelFor(DeveloperSettingsTap.Quiet, unlockedAfterTap = true),
        )
    }

    @Test
    fun `feedback clears before the run it belongs to expires`() {
        // The row goes quiet while a run may still be live, never the other way
        // round: a count that outlived its own run would be a lie on screen.
        assertTrue(DEVELOPER_SETTINGS_LABEL_REVERT_MS < DEVELOPER_SETTINGS_TAP_WINDOW_MS)
    }
}
