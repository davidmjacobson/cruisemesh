package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.DigestEntry

class DigestSyncTest {

    private fun userId(byte: Int): ByteArray = ByteArray(16) { byte.toByte() }

    @Test
    fun `chatId matching the HELLO'd userId is expected`() {
        val alice = userId(1)
        assertTrue(DigestSync.isExpectedChatId(digestChatId = alice, helloUserId = alice))
    }

    @Test
    fun `chatId that differs from the HELLO'd userId is rejected`() {
        val alice = userId(1)
        val bob = userId(2)
        assertFalse(DigestSync.isExpectedChatId(digestChatId = bob, helloUserId = alice))
    }

    @Test
    fun `a digest before any HELLO on the link is rejected`() {
        assertFalse(DigestSync.isExpectedChatId(digestChatId = userId(1), helloUserId = null))
    }

    @Test
    fun `matching is by content not reference`() {
        // Distinct ByteArray instances with the same bytes must match --
        // one comes off the wire (parseFrame), the other from MeshRouterState.
        val digestChatId = userId(1)
        val helloUserId = userId(1)
        assertTrue(digestChatId !== helloUserId)
        assertTrue(DigestSync.isExpectedChatId(digestChatId, helloUserId))
    }

    @Test
    fun `no entry for our userId means send everything`() {
        val ownUserId = userId(1)
        val entries = listOf(DigestEntry(senderUserId = userId(2), throughLamport = 9uL))
        assertEquals(0uL, DigestSync.throughLamportForSelf(entries, ownUserId))
    }

    @Test
    fun `an empty digest also means send everything`() {
        assertEquals(0uL, DigestSync.throughLamportForSelf(emptyList(), userId(1)))
    }

    @Test
    fun `the entry for our userId reports what the peer already has`() {
        val ownUserId = userId(1)
        val entries = listOf(
            DigestEntry(senderUserId = userId(2), throughLamport = 100uL),
            DigestEntry(senderUserId = ownUserId, throughLamport = 7uL),
        )
        assertEquals(7uL, DigestSync.throughLamportForSelf(entries, ownUserId))
    }

    @Test
    fun `entries about other senders are ignored, not summed or confused`() {
        val ownUserId = userId(1)
        val entries = listOf(
            DigestEntry(senderUserId = userId(2), throughLamport = 1uL),
            DigestEntry(senderUserId = userId(3), throughLamport = 2uL),
            DigestEntry(senderUserId = userId(4), throughLamport = 3uL),
        )
        assertEquals(0uL, DigestSync.throughLamportForSelf(entries, ownUserId))
    }
}
