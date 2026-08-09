package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.decodeMessageBody
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.openMessage

class ReceiptRelayRoundTripTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }
    }

    @Test
    fun `relay receipt round trip advances sender delivered and read watermarks without persisting receipt messages`() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceContact = contactFor(bob, "Bob")
        val bobContact = contactFor(alice, "Alice")
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(aliceContact)
        bobStore.upsertContact(bobContact)

        val textEnvelope = aliceStore.authorPairwiseMessage(
            alice,
            aliceContact,
            1u,
            "relay-text".toByteArray(),
            null,
            1_700_000_000_000L,
        ).envelope

        val openedText = openMessage(bob, textEnvelope.sealed)
        val decodedText = decodeMessageBody(openedText.payload)
        bobStore.insertMessage(
            StoredMessage(
                chatId = openedText.senderUserId,
                senderUserId = openedText.senderUserId,
                lamport = decodedText.lamport,
                timestamp = decodedText.timestamp,
                kind = decodedText.kind,
                payload = decodedText.content,
            ),
        )

        val through = bobStore.highestContiguousLamport(alice.userId, alice.userId)
        assertEquals(1uL, through)
        bobStore.recordOutgoingReceipt(alice.userId, alice.userId, 1u, through)
        bobStore.recordOutgoingReceipt(alice.userId, alice.userId, 2u, through)

        val deliveredEnvelope = bobStore.ensureAuthoredReceipt(
            bob,
            bobContact,
            alice.userId,
            1u,
            through,
            1_700_000_000_100L,
        ).envelope
        val readEnvelope = bobStore.ensureAuthoredReceipt(
            bob,
            bobContact,
            alice.userId,
            2u,
            through,
            1_700_000_000_200L,
        ).envelope

        assertEquals(
            1,
            bobStore.messagesForChat(alice.userId).size,
        )
        val actualEnvelopes = bobStore.pendingRelayOutgoingReceiptEnvelopes(10uL, 1_700_000_000_300L, emptyList())
        assertEquals(2, actualEnvelopes.size)
        assertEquals(deliveredEnvelope.toString(), actualEnvelopes[0].toString())
        assertEquals(readEnvelope.toString(), actualEnvelopes[1].toString())

        ingestReceiptEnvelope(aliceStore, alice, deliveredEnvelope)
        ingestReceiptEnvelope(aliceStore, alice, readEnvelope)

        assertEquals(1uL, aliceStore.receiptThrough(bob.userId, alice.userId, 1u))
        assertEquals(1uL, aliceStore.receiptThrough(bob.userId, alice.userId, 2u))
        assertEquals(1, aliceStore.messagesForChat(bob.userId).size)
    }

    private fun ingestReceiptEnvelope(
        store: MessageStore,
        recipient: Identity,
        envelope: uniffi.cruisemesh_core.OutgoingReceiptEnvelope,
    ) {
        val opened = openMessage(recipient, envelope.sealed)
        val body = decodeMessageBody(opened.payload)
        val receipt = decodeReceiptContent(body.content)
        store.recordReceipt(
            chatId = opened.senderUserId,
            senderUserId = recipient.userId,
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
            viaTransport = 2u, // relay-carried in this round-trip
            receivedAtMs = 1_700_000_000_000L,
        )
    }

    private fun contactFor(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = "https://relay.example.test",
        relayToken = "token-$name",
    )
}
