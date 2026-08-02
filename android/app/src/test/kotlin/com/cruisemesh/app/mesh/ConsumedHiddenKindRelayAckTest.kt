package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreRelayEnvelopeDisposition
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.generateIdentity
import java.io.File

/**
 * The relay-mailbox growth rule, end to end on the receive side.
 *
 * A receipt is the highest-volume kind on the wire (one delivered plus one
 * read watermark per message) and it leaves no `messages` row, so before the
 * consumed-hidden-kind set its relay copy could never be acked: the phone
 * consumed the envelope over Bluetooth first, the relay copy deduped as SEEN a
 * moment later, and the row then sat in the mailbox for its whole 7-day
 * expiry. A real mailbox reached ~29k rows this way.
 *
 * These tests drive the real [InboundEnvelopeProcessor] against an inert JVM
 * Context and then ask the real core for the ack decision, so the recording
 * side and the acking side are pinned together rather than in isolation. The
 * negative cases matter at least as much as the positive one: nothing muled,
 * nothing under a group's shared hint, and nothing we merely heard about may
 * ever be acked.
 */
class ConsumedHiddenKindRelayAckTest {

    private class FakePrefs : SharedPreferences {
        private val map = mutableMapOf<String, Any?>()

        override fun getAll(): MutableMap<String, *> = map
        override fun getString(key: String?, def: String?) = map[key] as? String ?: def
        @Suppress("UNCHECKED_CAST")
        override fun getStringSet(key: String?, def: MutableSet<String>?) =
            map[key] as? MutableSet<String> ?: def
        override fun getInt(key: String?, def: Int) = map[key] as? Int ?: def
        override fun getLong(key: String?, def: Long) = map[key] as? Long ?: def
        override fun getFloat(key: String?, def: Float) = map[key] as? Float ?: def
        override fun getBoolean(key: String?, def: Boolean) = map[key] as? Boolean ?: def
        override fun contains(key: String?) = map.containsKey(key)
        override fun edit(): SharedPreferences.Editor = Editor()
        override fun registerOnSharedPreferenceChangeListener(
            l: SharedPreferences.OnSharedPreferenceChangeListener?
        ) {}
        override fun unregisterOnSharedPreferenceChangeListener(
            l: SharedPreferences.OnSharedPreferenceChangeListener?
        ) {}

        inner class Editor : SharedPreferences.Editor {
            override fun putString(key: String?, value: String?) = apply { map[key!!] = value }
            override fun putStringSet(key: String?, value: MutableSet<String>?) =
                apply { map[key!!] = value }
            override fun putInt(key: String?, value: Int) = apply { map[key!!] = value }
            override fun putLong(key: String?, value: Long) = apply { map[key!!] = value }
            override fun putFloat(key: String?, value: Float) = apply { map[key!!] = value }
            override fun putBoolean(key: String?, value: Boolean) = apply { map[key!!] = value }
            override fun remove(key: String?) = apply { map.remove(key) }
            override fun clear() = apply { map.clear() }
            override fun commit() = true
            override fun apply() {}
        }
    }

    private class FakeContext : ContextWrapper(null) {
        private val prefs = mutableMapOf<String, FakePrefs>()
        private val files = File.createTempFile("hidden-acks", null).parentFile!!

        override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
            prefs.getOrPut(name ?: "") { FakePrefs() }
        override fun getApplicationContext(): Context = this
        override fun getFilesDir(): File = files
    }

    private val lanHooks = object : InboundEnvelopeProcessor.LanHooks {
        override fun sendLanEndpointHintTo(address: String) {}
        override fun connectToLanHint(hint: Frame.LanEndpoint, peerUserId: ByteArray) {}
        override fun saveLanEndpoint(
            networkId: String?,
            userId: ByteArray,
            endpoint: LanManualEndpoint,
        ) {}
        override fun currentLanNetworkId(): String? = null
        override fun onLanCapabilityChanged() {}
    }

    private fun contactFor(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = null,
        relayToken = null,
    )

    private fun processorFor(store: MessageStore, identity: Identity) = InboundEnvelopeProcessor(
        context = FakeContext(),
        store = store,
        identityProvider = { identity },
        requestRelaySync = {},
        lan = lanHooks,
    )

    private fun envelopeFrame(envelope: uniffi.cruisemesh_core.OutgoingReceiptEnvelope) =
        Frame.Envelope(
            msgId = envelope.msgId,
            hopTtl = envelope.hopTtl,
            expiry = envelope.expiry,
            recipientHint = envelope.recipientHint,
            sealed = envelope.sealed,
        )

    /**
     * Authors a real DELIVERED receipt from [sender] to [recipient], as the
     * sender's own relay-sync pass would: pairwise-sealed, addressed to the
     * recipient's own daily hint.
     */
    private fun receiptTo(
        senderStore: MessageStore,
        sender: Identity,
        recipient: Identity,
        now: Long,
    ) = senderStore.ensureAuthoredReceipt(
        sender,
        contactFor(recipient, "Recipient"),
        recipient.userId,
        RECEIPT_TYPE_DELIVERED,
        3uL,
        now,
    ).envelope

    @Test
    fun receiptConsumedOverBleIsAckedWhenItsRelayCopyTurnsUpAsSeen() {
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(contactFor(bob, "Bob"))
        bobStore.upsertContact(contactFor(alice, "Alice"))
        val now = System.currentTimeMillis()

        val receipt = receiptTo(aliceStore, alice, bob, now)
        val processor = processorFor(bobStore, bob)

        // Leg 1: it arrives over Bluetooth first, which is the ordinary case
        // and the whole reason the relay copy later looks like a duplicate.
        val overBle = processor.processInboundEnvelope("aa:bb:cc:dd:ee:ff", envelopeFrame(receipt), bob)
        assertEquals(CoreInboundDisposition.CONSUMED, overBle)
        assertTrue(
            "consuming a receipt as its sole endpoint must leave durable evidence",
            bobStore.consumedHiddenMsgIdRecorded(receipt.msgId, now),
        )

        // Leg 2: the relay hands the same envelope back on the next mailbox
        // walk. It dedupes to SEEN, exactly as in the field.
        val fromRelay = processor.handleRelayEnvelope(
            RelayFetchedEnvelope(
                id = 4_242L,
                msgId = receipt.msgId,
                hopTtl = receipt.hopTtl,
                recipientHint = receipt.recipientHint,
                sealed = receipt.sealed,
                expiryMs = receipt.expiry,
            ),
            bob,
        )
        assertEquals(CoreInboundDisposition.SEEN, fromRelay)

        val acked = bobStore.coreRelayAckIdsWithConsumed(
            listOf(
                CoreRelayEnvelopeDisposition(
                    relayId = 4_242L,
                    msgId = receipt.msgId,
                    disposition = fromRelay,
                    recipientHint = receipt.recipientHint,
                ),
            ),
            bob.userId,
            now,
        )
        assertEquals("the already-consumed relay copy must be deleted", listOf(4_242L), acked)
    }

    @Test
    fun aReceiptThisDeviceNeverConsumedIsNeverAcked() {
        // Same shape as above minus the consumption: this is what a copy we
        // only ever muled past looks like from the ack rule's point of view,
        // and its real recipient still needs it.
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(contactFor(bob, "Bob"))
        val now = System.currentTimeMillis()

        val receipt = receiptTo(aliceStore, alice, bob, now)
        assertFalse(bobStore.consumedHiddenMsgIdRecorded(receipt.msgId, now))

        val acked = bobStore.coreRelayAckIdsWithConsumed(
            listOf(
                CoreRelayEnvelopeDisposition(
                    relayId = 7L,
                    msgId = receipt.msgId,
                    disposition = CoreInboundDisposition.SEEN,
                    recipientHint = receipt.recipientHint,
                ),
            ),
            bob.userId,
            now,
        )
        assertTrue("nothing may be acked on no evidence", acked.isEmpty())
    }

    @Test
    fun anEnvelopeWeOnlyMuleRecordsNothingAndStaysOnTheRelay() {
        // Bob proxy-fetches mail addressed to Carol. He cannot open it, so it
        // goes to the carry queue: CARRIED, no record, never acked.
        val alice = generateIdentity()
        val bob = generateIdentity()
        val carol = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(contactFor(carol, "Carol"))
        bobStore.upsertContact(contactFor(carol, "Carol"))
        val now = System.currentTimeMillis()

        val receipt = receiptTo(aliceStore, alice, carol, now)
        val disposition = processorFor(bobStore, bob).handleRelayEnvelope(
            RelayFetchedEnvelope(
                id = 11L,
                msgId = receipt.msgId,
                hopTtl = receipt.hopTtl,
                recipientHint = receipt.recipientHint,
                sealed = receipt.sealed,
                expiryMs = receipt.expiry,
            ),
            bob,
        )
        assertEquals(CoreInboundDisposition.CARRIED, disposition)
        assertFalse(
            "a muled envelope must never look consumed",
            bobStore.consumedHiddenMsgIdRecorded(receipt.msgId, now),
        )
        assertTrue(
            bobStore.coreRelayAckIdsWithConsumed(
                listOf(
                    CoreRelayEnvelopeDisposition(
                        relayId = 11L,
                        msgId = receipt.msgId,
                        disposition = disposition,
                        recipientHint = receipt.recipientHint,
                    ),
                ),
                bob.userId,
                now,
            ).isEmpty(),
        )
    }

    @Test
    fun aConsumedHiddenKindIsStillNeverAckedUnderAGroupsSharedHint() {
        // The one shape that could otherwise slip through: the msg_id is
        // genuinely recorded, but the relay hands the row back addressed to a
        // group's shared hint, which every member fetches. The legacy
        // shared-row rule must beat the consumed evidence.
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(contactFor(bob, "Bob"))
        bobStore.upsertContact(contactFor(alice, "Alice"))
        val group: Group = createGroup("Family", listOf(alice.userId, bob.userId))
        bobStore.upsertGroup(group)
        val now = System.currentTimeMillis()

        val receipt = receiptTo(aliceStore, alice, bob, now)
        assertEquals(
            CoreInboundDisposition.CONSUMED,
            processorFor(bobStore, bob).processInboundEnvelope("aa:bb", envelopeFrame(receipt), bob),
        )
        assertTrue(bobStore.consumedHiddenMsgIdRecorded(receipt.msgId, now))

        val acked = bobStore.coreRelayAckIdsWithConsumed(
            listOf(
                CoreRelayEnvelopeDisposition(
                    relayId = 99L,
                    msgId = receipt.msgId,
                    disposition = CoreInboundDisposition.SEEN,
                    recipientHint = computeRecipientHint(group.id, now),
                ),
            ),
            bob.userId,
            now,
        )
        assertTrue("a shared group row is never ours alone to delete", acked.isEmpty())
    }

    @Test
    fun aChatMessageRecordsNothingHereAndStillAcksOnItsMessagesRow() {
        // Regression guard: chat kinds already leave a `messages` row, so they
        // must not also grow this table -- and the pre-existing ack path for
        // them must keep working untouched.
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        val aliceContact = aliceStore.upsertImportedContact(contactFor(bob, "Bob"))
        bobStore.upsertContact(contactFor(alice, "Alice"))
        val now = System.currentTimeMillis()

        val text = aliceStore.authorPairwiseMessage(
            alice,
            aliceContact,
            KIND_TEXT,
            "see you on deck 8".toByteArray(),
            null,
            now,
        ).envelope

        val processor = processorFor(bobStore, bob)
        // On-screen chat: skips MessageNotifier, whose Base64 call NPEs on the
        // JVM (same reason sibling tests do this); the notification path is
        // not what this test pins.
        ChatVisibility.setVisible(alice.userId)
        val overBle = try {
            processor.processInboundEnvelope(
                "aa:bb",
                Frame.Envelope(
                    msgId = text.msgId,
                    hopTtl = text.hopTtl,
                    expiry = text.expiry,
                    recipientHint = text.recipientHint,
                    sealed = text.sealed,
                ),
                bob,
            )
        } finally {
            ChatVisibility.reset()
        }
        assertEquals(CoreInboundDisposition.CONSUMED, overBle)
        assertFalse(
            "a chat message already has durable evidence; do not duplicate it",
            bobStore.consumedHiddenMsgIdRecorded(text.msgId, now),
        )

        val acked = bobStore.coreRelayAckIdsWithConsumed(
            listOf(
                CoreRelayEnvelopeDisposition(
                    relayId = 55L,
                    msgId = text.msgId,
                    disposition = CoreInboundDisposition.SEEN,
                    recipientHint = text.recipientHint,
                ),
            ),
            bob.userId,
            now,
        )
        assertEquals(listOf(55L), acked)
    }

    @Test
    fun recordedEvidenceIsPrunedOnceTheEnvelopeExpires() {
        // Bounded by construction: the record dies with the envelope it
        // vouches for, on the same prune the relay sync pass already runs.
        val alice = generateIdentity()
        val bob = generateIdentity()
        val aliceStore = MessageStore.open(":memory:")
        val bobStore = MessageStore.open(":memory:")
        aliceStore.upsertContact(contactFor(bob, "Bob"))
        bobStore.upsertContact(contactFor(alice, "Alice"))
        val now = System.currentTimeMillis()

        val receipt = receiptTo(aliceStore, alice, bob, now)
        processorFor(bobStore, bob).processInboundEnvelope("aa:bb", envelopeFrame(receipt), bob)
        assertTrue(bobStore.consumedHiddenMsgIdRecorded(receipt.msgId, now))

        assertEquals(0uL, bobStore.pruneExpiredConsumedHiddenMsgIds(now))
        assertEquals(1uL, bobStore.pruneExpiredConsumedHiddenMsgIds(receipt.expiry))
        assertFalse(bobStore.consumedHiddenMsgIdRecorded(receipt.msgId, now))
    }
}
