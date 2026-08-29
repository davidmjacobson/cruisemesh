package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PendingPeerMailTest {
    @Test
    fun aSendWithNowhereToGoMarksThatContact() {
        val mail = PendingPeerMail(ttlMs = 1_000L)
        mail.noteUnrouted(ALICE, 0L)

        assertTrue(mail.isWaiting(ALICE, 500L))
        assertFalse(mail.isWaiting(BOB, 500L))
    }

    @Test
    fun aLinkToThatContactClearsIt() {
        val mail = PendingPeerMail(ttlMs = 1_000L)
        mail.noteUnrouted(ALICE, 0L)

        mail.noteRouted(ALICE)

        assertFalse(mail.isWaiting(ALICE, 500L))
    }

    @Test
    fun anOldFailedSendStopsCountingAsMailWaiting() {
        // The latch RadioPowerPolicy learned about the hard way: a signal that
        // never expires is permanently true, and a permanently true priority
        // is no priority at all.
        val mail = PendingPeerMail(ttlMs = 1_000L)
        mail.noteUnrouted(ALICE, 0L)

        assertTrue(mail.isWaiting(ALICE, 1_000L))
        assertFalse(mail.isWaiting(ALICE, 1_001L))
    }

    @Test
    fun aLaterFailedSendRefreshesTheWindow() {
        val mail = PendingPeerMail(ttlMs = 1_000L)
        mail.noteUnrouted(ALICE, 0L)
        mail.noteUnrouted(ALICE, 900L)

        assertTrue(mail.isWaiting(ALICE, 1_500L))
    }

    @Test
    fun clearForgetsEverything() {
        val mail = PendingPeerMail(ttlMs = 1_000L)
        mail.noteUnrouted(ALICE, 0L)
        mail.noteUnrouted(BOB, 0L)

        mail.clear()

        assertFalse(mail.isWaiting(ALICE, 0L))
        assertFalse(mail.isWaiting(BOB, 0L))
    }

    private companion object {
        const val ALICE = "a11ce"
        const val BOB = "b0b"
    }
}
