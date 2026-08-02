package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.relayCursorKey

/**
 * The per-process record of contacts whose card relay endpoint has stopped
 * answering. The thresholds and windows are core's
 * (core/src/contact_relay_health.rs); what this pins is the state machine the
 * shell wraps around them — most of all that a rest belongs to an address and
 * dies with it.
 */
class ContactRelaySilenceTest {
    private val alice = "alice"
    private val dead = relayCursorKey("https://dead.example", "tok")
    private val live = relayCursorKey("https://live.example", "tok")
    private val now = 1_800_000_000_000L
    private val restWindow = 30 * 60 * 1000L

    private fun restedSilence(): ContactRelaySilence {
        val silence = ContactRelaySilence()
        assertEquals(1L, silence.noteSilentPass(alice, dead, true, now))
        assertEquals(2L, silence.noteSilentPass(alice, dead, true, now))
        return silence
    }

    @Test
    fun `an endpoint nobody has heard from is rested after two passes`() {
        val silence = ContactRelaySilence()
        assertTrue("nothing observed yet", silence.endpointAnswering(alice, dead, now))
        silence.noteSilentPass(alice, dead, true, now)
        assertTrue("one silent pass is not enough", silence.endpointAnswering(alice, dead, now))
        silence.noteSilentPass(alice, dead, true, now)
        assertFalse("two are", silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `silence without proof of working internet is not recorded at all`() {
        // The shell must not decide this for itself: the observation goes to
        // the core with the real value and the core returns a zero delta. A
        // phone in a tunnel fails every endpoint at once, and resting them all
        // would take the relay path away from the whole contact list.
        val silence = ContactRelaySilence()
        assertNull(silence.noteSilentPass(alice, dead, false, now))
        assertNull(silence.noteSilentPass(alice, dead, false, now))
        assertTrue(silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `a rested endpoint is probed again once the window is up`() {
        val silence = restedSilence()
        assertFalse(silence.endpointAnswering(alice, dead, now + restWindow - 1))
        assertTrue(silence.endpointAnswering(alice, dead, now + restWindow))
    }

    @Test
    fun `moving the contact to a different endpoint ends the rest immediately`() {
        // A new friend card or a T23 relay-update notice that changes the
        // address clears the persisted rejection streak in core; this is the
        // same rule for the unpersisted silence rest. Without it a contact who
        // migrated to a working host would keep being skipped for up to half
        // an hour after the repair arrived.
        val silence = restedSilence()
        assertTrue("the new host has never been tried", silence.endpointAnswering(alice, live, now))
        // And the stale verdict is genuinely gone, not merely bypassed: going
        // back to the old address starts from zero rather than resuming a
        // streak that was about something else.
        assertTrue(silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `re-stating the same endpoint keeps the rest`() {
        // Re-sharing a card from a phone whose config never changed carries
        // the SAME dead endpoint. Clearing for that would restart the
        // hammering and make the repair look like it worked, exactly as core
        // refuses to launder a rejection streak for it.
        val silence = restedSilence()
        assertFalse(silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `a streak from a previous address does not carry over to the new one`() {
        val silence = restedSilence()
        assertEquals("the new address starts from zero", 1L, silence.noteSilentPass(alice, live, true, now))
        assertTrue(silence.endpointAnswering(alice, live, now))
    }

    @Test
    fun `an endpoint that answers settles the question outright`() {
        val silence = restedSilence()
        silence.noteAnswered(alice)
        assertTrue(silence.endpointAnswering(alice, dead, now))
    }
}
