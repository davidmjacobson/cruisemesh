package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayFault
import uniffi.cruisemesh_core.coreContactRelayEndpointUsable
import uniffi.cruisemesh_core.coreContactRelayStreakDelta
import uniffi.cruisemesh_core.coreContactRelayUnreachableDelta
import uniffi.cruisemesh_core.coreContactRelayUnreachableEndpointUsable
import uniffi.cruisemesh_core.relayClassifyHttpError

/**
 * Pins the shell-side composition of the two ways a contact's friend-card
 * relay endpoint dies, as [RelaySyncEngine] applies them. The thresholds and
 * windows themselves are pinned in the core's own tests
 * (core/src/contact_relay_health.rs); what this file guards is that the shell
 * asks both questions and combines them the way the core intends, because
 * answering only the first one is exactly the bug.
 */
class ContactRelayHealthPolicyTest {
    private val now = 1_800_000_000_000L

    /** [RelaySyncEngine.resolvedPollRelayConfig]'s gate, in one place. */
    private fun pollable(
        rejectStreak: Long,
        rejectedAtMs: Long,
        silentStreak: Long,
        restedAtMs: Long,
        nowMs: Long = now,
    ): Boolean =
        coreContactRelayEndpointUsable(rejectStreak, rejectedAtMs, nowMs) &&
            coreContactRelayUnreachableEndpointUsable(silentStreak, restedAtMs, nowMs)

    @Test
    fun `a healthy endpoint is left entirely alone`() {
        assertTrue(pollable(0, 0, 0, 0))
    }

    @Test
    fun `a retired host is rested even though it never answers with a status`() {
        // The half of the field report that classification alone cannot
        // reach: a revoked token replies 401, but a retired host replies
        // nothing at all. Before this the transport failure carried no
        // signal, so the dead address was re-dialled on every pass forever.
        assertTrue("one silent pass is not enough", pollable(0, 0, 1, now))
        assertFalse("two are", pollable(0, 0, 2, now))
    }

    @Test
    fun `silence only counts when a different relay answered in the same pass`() {
        // The guard that keeps a phone in a tunnel from resting every
        // contact's endpoint at once. If this ever returns 1, one flight
        // takes the relay path away from the whole contact list.
        assertEquals(0L, coreContactRelayUnreachableDelta(false))
        assertEquals(1L, coreContactRelayUnreachableDelta(true))
    }

    @Test
    fun `a rejection needs no such proof because the endpoint spoke`() {
        // Deliberately asymmetric with the silence rule above: a 401 is the
        // endpoint itself disowning the credential, so it is evidence about
        // the card whatever the rest of the network is doing.
        assertEquals(1L, coreContactRelayStreakDelta(CoreRelayFault.TOKEN_REJECTED))
        assertEquals(1L, coreContactRelayStreakDelta(relayClassifyHttpError(401.toUShort(), null)))
        assertEquals(1L, coreContactRelayStreakDelta(relayClassifyHttpError(403.toUShort(), null)))
    }

    @Test
    fun `a busy service is never mistaken for a dead card`() {
        // A full mailbox or a rate limit is a healthy card behind a busy
        // relay. Writing the card off for those would strand a contact whose
        // family merely filled their storage.
        for (fault in listOf(
            CoreRelayFault.RATE_LIMITED,
            CoreRelayFault.MAILBOX_FULL,
            CoreRelayFault.MESSAGE_TOO_LARGE,
            CoreRelayFault.OUTAGE,
        )) {
            assertEquals("$fault must stay retryable", 0L, coreContactRelayStreakDelta(fault))
        }
    }

    @Test
    fun `either kind of write-off suppresses polling on its own`() {
        // The poll path has no fallback, so both gates have to be able to
        // close it independently.
        assertFalse("a rejected card", pollable(2, now, 0, 0))
        assertFalse("a silent host", pollable(0, 0, 2, now))
    }

    @Test
    fun `both kinds of write-off recover unattended, on their own clocks`() {
        // Neither state may be permanent: a re-provisioned family and a
        // rebooted host both have to heal with nobody touching the phone.
        val day = 24 * 60 * 60 * 1000L
        assertTrue("a rejected card re-probes eventually", pollable(2, now, 0, 0, now + day))
        assertTrue("a silent host re-probes sooner", pollable(0, 0, 2, now, now + day))
        // And the silent host comes back first -- it usually fixes itself,
        // while a rejected card waits on a person re-sharing it.
        val hour = 60 * 60 * 1000L
        assertTrue("a silent host is retried within the hour", pollable(0, 0, 2, now, now + hour))
        assertFalse("a rejected card is not", pollable(2, now, 0, 0, now + hour))
    }
}
