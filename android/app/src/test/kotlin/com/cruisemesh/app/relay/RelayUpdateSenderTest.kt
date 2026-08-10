package com.cruisemesh.app.relay

import com.cruisemesh.app.mesh.HostCoreLibrary
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.decodeExtendedMessageBody
import uniffi.cruisemesh_core.decodeRelayUpdateContent
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.openMessage

class RelayUpdateSenderTest {
    companion object {
        private const val RELAY_URL = "https://relay.example"
        private const val DEPOSIT_TOKEN = "cmdep1-63hWvx1kHLKirfl9GV576eAi_rURpyZixpsCVUCXNJk"
        private const val KIND_RELAY_UPDATE: UByte = 9u

        init {
            HostCoreLibrary.load()
        }
    }

    @Test
    fun passFanoutQueuesOneOrderedUpdateThatRepairsTheContactsStoredEndpoint() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(contactFor(bob, "Bob"))
        bobStore.upsertContact(contactFor(alice, "Alice"))

        val epoch = 42L

        var syncRequests = 0
        RelayUpdateSender.queueToAllContacts(
            store = aliceStore,
            identity = alice,
            epoch = epoch,
            relay = RelayConfig(RELAY_URL, DEPOSIT_TOKEN),
            sendToUser = { _, _ -> false },
            requestSync = { syncRequests += 1 },
        )

        val bodies = aliceStore
            .outboundEnvelopesAfter(bob.userId, alice.userId, 0uL)
            .map { decodeExtendedMessageBody(openMessage(bob, it.sealed).payload) }
        val updates = bodies.filter { it.kind == KIND_RELAY_UPDATE }
        assertEquals(1, updates.size)
        assertEquals(1, syncRequests)

        val update = decodeRelayUpdateContent(updates.single().content)
        assertArrayEquals(alice.userId, update.subjectUserId)
        assertEquals(epoch, update.relayEpoch)
        assertEquals(RELAY_URL, update.relayUrl)
        assertEquals(DEPOSIT_TOKEN, update.relayToken)

        assertTrue(bobStore.applyContactRelayUpdate(alice.userId, update))
        val repaired = bobStore.getContact(alice.userId)!!
        assertEquals(RELAY_URL, repaired.relayUrl)
        assertEquals(DEPOSIT_TOKEN, repaired.relayToken)
    }

    private fun contactFor(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = null,
        relayToken = null,
    )
}
