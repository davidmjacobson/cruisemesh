package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope

class OwnReceiptSpraySelectorTest {

    private val peer = ByteArray(16) { 0xAA.toByte() }
    private val otherSender = ByteArray(16) { 0xBB.toByte() }

    private fun receipt(
        msgId: Int,
        recipientUserId: ByteArray = otherSender,
        expiry: Long = 1_000L,
        sealedSize: Int = 10,
    ): OutgoingReceiptEnvelope = OutgoingReceiptEnvelope(
        msgId = byteArrayOf(msgId.toByte()),
        recipientUserId = recipientUserId,
        chatId = recipientUserId,
        senderUserId = ByteArray(16) { 0x03 },
        receiptType = 1u,
        throughLamport = msgId.toULong(),
        timestamp = 0L,
        hopTtl = 7u,
        expiry = expiry,
        recipientHint = ByteArray(8),
        sealed = ByteArray(sealedSize),
    )

    @Test
    fun `nothing pending selects nothing`() {
        val result = OwnReceiptSpraySelector.select(
            pending = emptyList(),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        assertTrue(result.isEmpty())
    }

    @Test
    fun `selects everything when nothing is excluded, expired, or over budget`() {
        val a = receipt(1)
        val b = receipt(2)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(a, b),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        assertEquals(listOf(a, b), result)
    }

    @Test
    fun `excludes receipts owed to the digest peer itself`() {
        val toPeer = receipt(1, recipientUserId = peer)
        val toOther = receipt(2, recipientUserId = otherSender)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(toPeer, toOther),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        // syncReceiptsFirst already handed `toPeer` to the peer directly.
        assertEquals(listOf(toOther), result)
    }

    @Test
    fun `excludes expired receipts`() {
        val fresh = receipt(1, expiry = 2_000L)
        val expired = receipt(2, expiry = 500L)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(fresh, expired),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 1_000L,
            budgetBytes = 1024L,
        )
        assertEquals(listOf(fresh), result)
    }

    @Test
    fun `a receipt expiring exactly now counts as expired`() {
        val boundary = receipt(1, expiry = 1_000L)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(boundary),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 1_000L,
            budgetBytes = 1024L,
        )
        assertTrue(result.isEmpty())
    }

    @Test
    fun `excludes receipts the peer already carries by msg_id`() {
        val known = receipt(1)
        val unknown = receipt(2)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(known, unknown),
            peerUserId = peer,
            peerKnownMsgIdsHex = setOf(UserIdHex.encode(known.msgId)),
            nowMs = 0L,
            budgetBytes = 1024L,
        )
        assertEquals(listOf(unknown), result)
    }

    @Test
    fun `stops entirely once the byte budget would be exceeded, oldest-first`() {
        val first = receipt(1, sealedSize = 60)
        val second = receipt(2, sealedSize = 60)
        val third = receipt(3, sealedSize = 60)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(first, second, third),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 100L,
        )
        // Only `first` fits; `second` would push used bytes to 120 > 100.
        assertEquals(listOf(first), result)
    }

    @Test
    fun `a receipt exactly at the budget is included`() {
        val exact = receipt(1, sealedSize = 100)
        val result = OwnReceiptSpraySelector.select(
            pending = listOf(exact),
            peerUserId = peer,
            peerKnownMsgIdsHex = emptySet(),
            nowMs = 0L,
            budgetBytes = 100L,
        )
        assertEquals(listOf(exact), result)
    }
}
