package com.cruisemesh.app.mesh

import com.cruisemesh.app.ui.PeerEvidence
import com.cruisemesh.app.ui.latestPeerStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.generateIdentity

/**
 * "Received your message" on the Connection details screen has to mean a
 * message a person wrote, not the service traffic the app sends on its own.
 *
 * The shell no longer records that event: it hands core the receipt and the
 * moment it arrived, and core decides. These drive the same store call
 * [InboundEnvelopeProcessor.handleIncomingReceipt] makes and assert the line
 * the screen would actually draw, so the shell can never quietly grow a second
 * recording site that reports service traffic as a delivered message.
 */
class DeliveredEvidenceFromReceiptTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }

        private const val ARRIVED_AT = 1_700_000_000_000L
        private const val KIND_TEXT: UByte = 1u
        private const val KIND_FRIEND_DIRECTORY: UByte = 6u
        private const val RECEIPT_DELIVERED: UByte = 1u

        /** BLE direct, in the [uniffi.cruisemesh_core.MessageArrival] encoding. */
        private const val VIA_BLUETOOTH: UByte = 0u
    }

    @Test
    fun `a receipt acking a real message reads as a delivered message`() {
        val (store, me, friend) = chatWithAuthoredKind(KIND_TEXT)

        store.recordReceipt(
            chatId = friend.userId,
            senderUserId = me.userId,
            receiptType = RECEIPT_DELIVERED,
            throughLamport = 1uL,
            viaTransport = VIA_BLUETOOTH,
            receivedAtMs = ARRIVED_AT,
        )

        val line = latestPeerStatus(store.peerConnectionSummaries())
        assertEquals(PeerEvidence.MESSAGE_DELIVERED, line?.evidence)
        assertEquals(PeerConnectionTransport.BLUETOOTH, line?.transport)
        assertEquals(ARRIVED_AT, line?.atMs)
    }

    @Test
    fun `a receipt acking only a friend-directory blob reads as no evidence at all`() {
        val (store, me, friend) = chatWithAuthoredKind(KIND_FRIEND_DIRECTORY)

        store.recordReceipt(
            chatId = friend.userId,
            senderUserId = me.userId,
            receiptType = RECEIPT_DELIVERED,
            throughLamport = 1uL,
            viaTransport = VIA_BLUETOOTH,
            receivedAtMs = ARRIVED_AT,
        )

        assertNull(latestPeerStatus(store.peerConnectionSummaries()))
    }

    /** A store where this device has authored exactly one message of [kind] to one friend. */
    private fun chatWithAuthoredKind(kind: UByte): Triple<MessageStore, Identity, Identity> {
        val me = generateIdentity()
        val friend = generateIdentity()
        val store = MessageStore.open(":memory:")
        val contact = Contact(
            userId = friend.userId,
            name = "Friend",
            signPk = friend.signPk,
            agreePk = friend.agreePk,
            relayUrl = null,
            relayToken = null,
        )
        store.upsertContact(contact)
        store.authorPairwiseMessage(me, contact, kind, "payload".toByteArray(), null, ARRIVED_AT)
        return Triple(store, me, friend)
    }
}
