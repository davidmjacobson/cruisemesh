package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.OutboundEnvelope

class OwnOutboundSpraySelectorTest {

    private fun envelope(
        msgId: Int,
        expiry: Long = 1_000L,
        sealedSize: Int = 10,
    ): OutboundEnvelope = OutboundEnvelope(
        msgId = byteArrayOf(msgId.toByte()),
        recipientUserId = ByteArray(16) { 0x01 },
        chatId = ByteArray(16) { 0x02 },
        senderUserId = ByteArray(16) { 0x03 },
        kind = 1u,
        lamport = msgId.toULong(),
        timestamp = 0L,
        hopTtl = 7u,
        expiry = expiry,
        recipientHint = ByteArray(8),
        sealed = ByteArray(sealedSize),
    )

    @Test
    fun `nothing pending selects nothing`() {
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = emptyList(),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        assertTrue(result.isEmpty())
    }

    @Test
    fun `selects everything when nothing is expired, known, or over budget`() {
        val a = envelope(1)
        val b = envelope(2)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(a, b)),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        assertEquals(listOf(a, b), result)
    }

    @Test
    fun `excludes expired envelopes`() {
        val fresh = envelope(1, expiry = 2_000L)
        val expired = envelope(2, expiry = 500L)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(fresh, expired)),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 1_000L,
            budgetBytes = 1024L,
        )
        assertEquals(listOf(fresh), result)
    }

    @Test
    fun `an envelope expiring exactly now counts as expired`() {
        val boundary = envelope(1, expiry = 1_000L)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(boundary)),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 1_000L,
            budgetBytes = 1024L,
        )
        assertTrue(result.isEmpty())
    }

    @Test
    fun `excludes envelopes the peer already carries by msg_id`() {
        val known = envelope(1)
        val unknown = envelope(2)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(known, unknown)),
            peerKnownMsgIdsHex = setOf(com.cruisemesh.app.chat.UserIdHex.encode(known.msgId)),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        assertEquals(listOf(unknown), result)
    }

    @Test
    fun `stops entirely once the byte budget would be exceeded, oldest-first`() {
        val first = envelope(1, sealedSize = 60)
        val second = envelope(2, sealedSize = 60)
        val third = envelope(3, sealedSize = 60)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(first, second, third)),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 100L,
        )
        // Only `first` fits; `second` would push used bytes to 120 > 100, so
        // the walk stops there rather than skipping to `third`.
        assertEquals(listOf(first), result)
    }

    @Test
    fun `budget is shared across recipients in the given order, not per-recipient`() {
        val fromAlice = envelope(1, sealedSize = 80)
        val fromBob = envelope(2, sealedSize = 80)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(fromAlice), listOf(fromBob)),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 100L,
        )
        // Alice's envelope alone fits (80 <= 100); Bob's would push to 160,
        // so the walk stops before ever looking at Bob's mail this pass.
        assertEquals(listOf(fromAlice), result)
    }

    @Test
    fun `an envelope exactly at the budget is included`() {
        val exact = envelope(1, sealedSize = 100)
        val result = OwnOutboundSpraySelector.select(
            pendingByRecipient = listOf(listOf(exact)),
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 100L,
        )
        assertEquals(listOf(exact), result)
    }
}
