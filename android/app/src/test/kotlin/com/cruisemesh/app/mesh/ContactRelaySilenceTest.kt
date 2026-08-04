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

    @Test
    fun `an address that just failed is not dialled again for the rest of the pass`() {
        // The whole point of the pass-local arm. A rest needs two passes, so
        // before this the first failure taught the pass nothing and a backlog
        // of queued envelopes re-dialled the same dead host once each -- 352
        // TLS handshakes in 27 seconds in the field report this came from.
        val silence = ContactRelaySilence()
        silence.beginPass()
        assertTrue("never tried yet", silence.endpointAnswering(alice, dead, now))
        assertTrue("first failure is news", silence.noteUnreachableThisPass(alice, dead))
        assertFalse("every later envelope this pass skips it", silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `only the first failure per address in a pass is worth logging`() {
        val silence = ContactRelaySilence()
        silence.beginPass()
        assertTrue(silence.noteUnreachableThisPass(alice, dead))
        assertFalse("same address again says nothing new", silence.noteUnreachableThisPass(alice, dead))
        assertTrue("a different address is its own news", silence.noteUnreachableThisPass(alice, live))
    }

    @Test
    fun `a card that moves the contact mid-pass is tried immediately`() {
        // Same rule as the rest window: a host that has never been dialled
        // cannot have been silent, so a T23 notice or a fresh card arriving
        // between two envelopes must not serve out the old address's skip.
        val silence = ContactRelaySilence()
        silence.beginPass()
        silence.noteUnreachableThisPass(alice, dead)
        assertTrue(silence.endpointAnswering(alice, live, now))
    }

    @Test
    fun `the pass-local skip does not survive into the next pass`() {
        // It is not a rest and must not act like one: one failed pass is
        // explicitly not enough to write an endpoint off, so the next pass
        // owes it a fresh probe.
        val silence = ContactRelaySilence()
        silence.beginPass()
        silence.noteUnreachableThisPass(alice, dead)
        assertEquals(listOf(alice to 1L), silence.commitPass(true, now))
        silence.beginPass()
        assertTrue("one silent pass is still not enough", silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `two silent passes still rest the endpoint`() {
        // The pass-local arm must not change what the streak means: this is
        // the pre-existing two-pass behaviour, now driven through commitPass.
        val silence = ContactRelaySilence()
        for (expected in listOf(1L, 2L)) {
            silence.beginPass()
            silence.noteUnreachableThisPass(alice, dead)
            assertEquals(listOf(alice to expected), silence.commitPass(true, now))
        }
        silence.beginPass()
        assertFalse(silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `silence committed without proof of working internet rests nobody`() {
        // A phone in a tunnel fails every endpoint at once. The pass-local
        // skip still saves the redundant dials inside that pass, but it must
        // not harden into a rest that takes the relay path away from the whole
        // contact list once connectivity returns.
        val silence = ContactRelaySilence()
        silence.beginPass()
        silence.noteUnreachableThisPass(alice, dead)
        assertEquals(emptyList<Pair<String, Long>>(), silence.commitPass(false, now))
        silence.beginPass()
        assertTrue(silence.endpointAnswering(alice, dead, now))
    }

    @Test
    fun `an endpoint that answers later in the pass is dialled again`() {
        // Recorded silence is provisional until the pass ends, so a success
        // against the same address -- a host that was mid-reboot -- has to
        // clear it outright rather than leave the rest of the pass skipping.
        val silence = ContactRelaySilence()
        silence.beginPass()
        silence.noteUnreachableThisPass(alice, dead)
        silence.noteAnswered(alice)
        assertTrue(silence.endpointAnswering(alice, dead, now))
        assertEquals("and nothing is left to commit", emptyList<Pair<String, Long>>(), silence.commitPass(true, now))
    }
}
