package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RadioPowerPolicyTest {

    // -- shouldEscalate ------------------------------------------------

    @Test
    fun `screen on with zero links escalates`() {
        val inputs = RadioPowerInputs(
            screenInteractive = true,
            liveLinkCount = 0,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        assertTrue(RadioPowerPolicy.shouldEscalate(inputs))
    }

    @Test
    fun `screen off with zero links and no other trigger does not escalate`() {
        val inputs = RadioPowerInputs(
            screenInteractive = false,
            liveLinkCount = 0,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        assertFalse(RadioPowerPolicy.shouldEscalate(inputs))
    }

    @Test
    fun `screen on with a live link and no other trigger does not escalate`() {
        val inputs = RadioPowerInputs(
            screenInteractive = true,
            liveLinkCount = 1,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        assertFalse(RadioPowerPolicy.shouldEscalate(inputs))
    }

    @Test
    fun `a recent link change escalates even with a live link and the screen off`() {
        val inputs = RadioPowerInputs(
            screenInteractive = false,
            liveLinkCount = 1,
            msSinceLastLinkChange = 1_000L,
            carryQueueHasUnlinkedMail = false,
        )
        assertTrue(RadioPowerPolicy.shouldEscalate(inputs))
    }

    @Test
    fun `a link change outside the escalation window does not escalate on its own`() {
        val inputs = RadioPowerInputs(
            screenInteractive = false,
            liveLinkCount = 1,
            msSinceLastLinkChange = RadioPowerPolicy.ESCALATION_WINDOW_MS + 1,
            carryQueueHasUnlinkedMail = false,
        )
        assertFalse(RadioPowerPolicy.shouldEscalate(inputs))
    }

    @Test
    fun `a link change exactly at the escalation window boundary still escalates`() {
        val inputs = RadioPowerInputs(
            screenInteractive = false,
            liveLinkCount = 1,
            msSinceLastLinkChange = RadioPowerPolicy.ESCALATION_WINDOW_MS,
            carryQueueHasUnlinkedMail = false,
        )
        assertTrue(RadioPowerPolicy.shouldEscalate(inputs))
    }

    @Test
    fun `carried mail for an unlinked contact escalates on its own`() {
        val inputs = RadioPowerInputs(
            screenInteractive = false,
            liveLinkCount = 1,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = true,
        )
        assertTrue(RadioPowerPolicy.shouldEscalate(inputs))
    }

    // -- nextDutyMode (dwell/hysteresis) --------------------------------

    @Test
    fun `escalate upshifts from LOW_POWER immediately`() {
        val next = RadioPowerPolicy.nextDutyMode(RadioDutyMode.LOW_POWER, escalate = true, msSinceModeChanged = 0L)
        assertEquals(RadioDutyMode.BALANCED, next)
    }

    @Test
    fun `escalate keeps BALANCED unchanged`() {
        val next = RadioPowerPolicy.nextDutyMode(RadioDutyMode.BALANCED, escalate = true, msSinceModeChanged = 0L)
        assertEquals(RadioDutyMode.BALANCED, next)
    }

    @Test
    fun `no escalate does not downshift before the minimum dwell`() {
        val next = RadioPowerPolicy.nextDutyMode(
            RadioDutyMode.BALANCED,
            escalate = false,
            msSinceModeChanged = RadioPowerPolicy.MIN_DWELL_MS - 1,
        )
        assertEquals(RadioDutyMode.BALANCED, next)
    }

    @Test
    fun `no escalate downshifts once the minimum dwell has elapsed`() {
        val next = RadioPowerPolicy.nextDutyMode(
            RadioDutyMode.BALANCED,
            escalate = false,
            msSinceModeChanged = RadioPowerPolicy.MIN_DWELL_MS,
        )
        assertEquals(RadioDutyMode.LOW_POWER, next)
    }

    @Test
    fun `no escalate keeps LOW_POWER unchanged regardless of dwell`() {
        val next = RadioPowerPolicy.nextDutyMode(RadioDutyMode.LOW_POWER, escalate = false, msSinceModeChanged = 999_999L)
        assertEquals(RadioDutyMode.LOW_POWER, next)
    }

    // -- evaluate (stateful integration) --------------------------------

    @Test
    fun `evaluate starts at LOW_POWER by default`() {
        val policy = RadioPowerPolicy()
        val quiet = RadioPowerInputs(
            screenInteractive = false,
            liveLinkCount = 1,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        assertEquals(RadioDutyMode.LOW_POWER, policy.evaluate(quiet, nowMs = 0L))
    }

    @Test
    fun `evaluate escalates immediately on a lonely-and-awake trigger`() {
        val policy = RadioPowerPolicy()
        val lonely = RadioPowerInputs(
            screenInteractive = true,
            liveLinkCount = 0,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        assertEquals(RadioDutyMode.BALANCED, policy.evaluate(lonely, nowMs = 1_000L))
    }

    @Test
    fun `evaluate does not downshift until the dwell elapses after escalation`() {
        val policy = RadioPowerPolicy()
        val lonely = RadioPowerInputs(
            screenInteractive = true,
            liveLinkCount = 0,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        val quiet = lonely.copy(screenInteractive = false, liveLinkCount = 1)

        assertEquals(RadioDutyMode.BALANCED, policy.evaluate(lonely, nowMs = 0L))
        // Condition clears immediately, but dwell has not elapsed yet.
        assertEquals(RadioDutyMode.BALANCED, policy.evaluate(quiet, nowMs = RadioPowerPolicy.MIN_DWELL_MS - 1))
        // Dwell has now elapsed with the escalation condition still clear.
        assertEquals(RadioDutyMode.LOW_POWER, policy.evaluate(quiet, nowMs = RadioPowerPolicy.MIN_DWELL_MS))
    }

    @Test
    fun `evaluate re-escalates immediately even right after a downshift`() {
        val policy = RadioPowerPolicy()
        val lonely = RadioPowerInputs(
            screenInteractive = true,
            liveLinkCount = 0,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        val quiet = lonely.copy(screenInteractive = false, liveLinkCount = 1)

        policy.evaluate(lonely, nowMs = 0L)
        assertEquals(RadioDutyMode.LOW_POWER, policy.evaluate(quiet, nowMs = RadioPowerPolicy.MIN_DWELL_MS))
        // A fresh trigger the instant after downshifting upshifts immediately again.
        assertEquals(RadioDutyMode.BALANCED, policy.evaluate(lonely, nowMs = RadioPowerPolicy.MIN_DWELL_MS + 1))
    }

    @Test
    fun `rapid flicker between triggered and clear never downshifts before its own dwell`() {
        // Regression guard for the Android 5-scans-per-30s throttle: once at
        // BALANCED, oscillating the escalate condition every tick must not
        // cause a downshift until MIN_DWELL_MS has actually elapsed since the
        // last real mode change.
        val policy = RadioPowerPolicy()
        val lonely = RadioPowerInputs(
            screenInteractive = true,
            liveLinkCount = 0,
            msSinceLastLinkChange = Long.MAX_VALUE / 2,
            carryQueueHasUnlinkedMail = false,
        )
        val quiet = lonely.copy(screenInteractive = false, liveLinkCount = 1)

        assertEquals(RadioDutyMode.BALANCED, policy.evaluate(lonely, nowMs = 0L))
        var t = 0L
        val step = RadioPowerPolicy.MIN_DWELL_MS / 10
        repeat(9) {
            t += step
            val inputs = if (it % 2 == 0) quiet else lonely
            assertEquals(RadioDutyMode.BALANCED, policy.evaluate(inputs, nowMs = t))
        }
    }

    // -- relayPollIntervalMs ---------------------------------------------

    @Test
    fun `no prior decision and currently healthy uses the healthy interval`() {
        assertEquals(
            RadioPowerPolicy.RELAY_POLL_HEALTHY_MS,
            RadioPowerPolicy.relayPollIntervalMs(previouslyHealthy = null, currentlyHealthy = true),
        )
    }

    @Test
    fun `no prior decision and currently unhealthy uses the unhealthy interval`() {
        assertEquals(
            RadioPowerPolicy.RELAY_POLL_UNHEALTHY_MS,
            RadioPowerPolicy.relayPollIntervalMs(previouslyHealthy = null, currentlyHealthy = false),
        )
    }

    @Test
    fun `staying healthy uses the healthy interval`() {
        assertEquals(
            RadioPowerPolicy.RELAY_POLL_HEALTHY_MS,
            RadioPowerPolicy.relayPollIntervalMs(previouslyHealthy = true, currentlyHealthy = true),
        )
    }

    @Test
    fun `staying unhealthy uses the unhealthy interval`() {
        assertEquals(
            RadioPowerPolicy.RELAY_POLL_UNHEALTHY_MS,
            RadioPowerPolicy.relayPollIntervalMs(previouslyHealthy = false, currentlyHealthy = false),
        )
    }

    @Test
    fun `healthy to down transition uses the short immediate interval`() {
        assertEquals(
            RadioPowerPolicy.RELAY_POLL_TRANSITION_MS,
            RadioPowerPolicy.relayPollIntervalMs(previouslyHealthy = true, currentlyHealthy = false),
        )
    }

    @Test
    fun `unhealthy to healthy transition uses the healthy interval, not the transition one`() {
        assertEquals(
            RadioPowerPolicy.RELAY_POLL_HEALTHY_MS,
            RadioPowerPolicy.relayPollIntervalMs(previouslyHealthy = false, currentlyHealthy = true),
        )
    }
}
