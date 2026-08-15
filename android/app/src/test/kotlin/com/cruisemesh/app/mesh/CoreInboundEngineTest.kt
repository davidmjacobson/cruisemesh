package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.SeenIds
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.makeFriendCard
import uniffi.cruisemesh_core.parseFrame
import uniffi.cruisemesh_core.sealGroupMessage
import uniffi.cruisemesh_core.sealMessage
import java.io.File

/**
 * The receive path running on the core engine.
 *
 * Two halves, because the migration makes two separate claims. The first half
 * drives [CoreInboundAdapter] directly and pins the mechanics: a frame reaches
 * `MessageStore.processInboundFrame`, and each bounded action it hands back --
 * the hop-decremented flood copy, the family-carry nudge, the post-delivery
 * commit -- is actually executed by the shell, in the DTN D4 order that makes a
 * failed delivery re-presentable rather than acked.
 *
 * The second half re-runs the highest-value existing disposition tests --
 * `BlockedSenderTest` and `GroupMembershipEnforcementTest` -- through the real
 * [InboundEnvelopeProcessor] with the engine flag flipped to
 * [InboundEngine.CORE]. Those tests own invariants (a blocked identity never
 * resurrects a contact; a leaked group key does not let a non-member's body
 * land) that must hold identically on both engines, and they are the reason
 * this is a behaviour-equivalence claim rather than a hope.
 */
class CoreInboundEngineTest {

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
        private val files = File.createTempFile("coreinbound", null).parentFile!!

        override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
            prefs.getOrPut(name ?: "") { FakePrefs() }
        override fun getApplicationContext(): Context = this
        override fun getFilesDir(): File = files
    }

    private val lanHooks = object : InboundEnvelopeProcessor.LanHooks {
        override fun sendLanEndpointHintTo(address: String) {}
        override fun connectToLanHint(hint: Frame.LanEndpoint, peerUserId: ByteArray) {}
        override fun saveHintedLanEndpoint(
            networkId: String?,
            userId: ByteArray,
            endpoint: LanManualEndpoint,
        ) {}
        override fun currentLanNetworkId(): String? = null
        override fun onLanCapabilityChanged() {}
    }

    /** Records every action the adapter asked the shell to take. */
    private class RecordingIntents(
        /** What [deliver] reports back, so a failed durable persist can be pinned. */
        var deliverySucceeds: Boolean = true,
    ) : CoreInboundAdapter.Intents {
        val flooded = mutableListOf<Pair<String?, ByteArray>>()
        val delivered = mutableListOf<ByteArray>()
        val deliveredGroupIds = mutableListOf<ByteArray?>()
        var familyCarries = 0

        override fun flood(sourceAddress: String?, frame: ByteArray) {
            flooded += sourceAddress to frame
        }

        override fun deliver(
            sourceAddress: String?,
            hopTtl: UByte,
            msgId: ByteArray,
            senderUserId: ByteArray,
            groupId: ByteArray?,
            payload: ByteArray,
            identity: Identity,
        ): Boolean {
            delivered += payload
            deliveredGroupIds += groupId
            return deliverySucceeds
        }

        override fun onFamilyCarry() {
            familyCarries += 1
        }
    }

    private fun contactOf(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = null,
        relayToken = null,
    )

    private fun sealedEnvelopeTo(
        sender: Identity,
        recipient: Identity,
        hintFor: ByteArray,
        now: Long,
        text: String = "hello",
        hopTtl: UByte = 7u,
    ) = Frame.Envelope(
        msgId = generateMsgId(),
        hopTtl = hopTtl,
        expiry = now + 60_000,
        recipientHint = computeRecipientHint(hintFor, now),
        sealed = sealMessage(
            sender,
            recipient.agreePk,
            encodeMessageBody(
                MessageBody(
                    kind = KIND_TEXT,
                    chatId = sender.userId,
                    lamport = 1u,
                    timestamp = now,
                    content = text.toByteArray(),
                ),
            ),
        ),
    )

    // ---- Half one: the adapter mechanics -------------------------------

    @Test
    fun aForeignEnvelopeIsCarriedByCoreAndFloodedByTheShell() {
        val me = generateIdentity()
        val sender = generateIdentity()
        val stranger = generateIdentity()
        val store = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        val intents = RecordingIntents()
        val adapter = CoreInboundAdapter(store, SeenIds(), intents) { now }

        val envelope = sealedEnvelopeTo(sender, stranger, stranger.userId, now)
        val disposition = adapter.process("AA:BB:CC:DD:EE:FF", envelope, me)

        assertEquals(CoreInboundDisposition.CARRIED, disposition)
        assertEquals("nothing addressed to us was delivered", 0, intents.delivered.size)
        assertEquals("core enqueued the mule copy", 1uL, store.carriedLen())
        assertEquals("the shell was asked to flood exactly one copy", 1, intents.flooded.size)

        val (excluded, frame) = intents.flooded.single()
        assertEquals("the arrival link is excluded from the fan-out", "AA:BB:CC:DD:EE:FF", excluded)
        val relayed = parseFrame(frame) as Frame.Envelope
        assertTrue(relayed.msgId.contentEquals(envelope.msgId))
        assertEquals("core hop-decremented the flood copy", 6.toUByte(), relayed.hopTtl)

        // A foreign envelope is bound for someone we do not know, so nothing
        // nudges the relay upload lane.
        assertEquals(0, intents.familyCarries)
    }

    @Test
    fun aFamilyClassifiedCarryNudgesTheRelayUploadLane() {
        val me = generateIdentity()
        val sender = generateIdentity()
        val friendOfMine = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(friendOfMine, "Friend"))
        val now = System.currentTimeMillis()
        val intents = RecordingIntents()
        val adapter = CoreInboundAdapter(store, SeenIds(), intents) { now }

        // Addressed to a contact of ours: core classifies the carry family, and
        // that classification -- not a shell re-derivation of it -- is what
        // decides the upload nudge.
        val envelope = sealedEnvelopeTo(sender, friendOfMine, friendOfMine.userId, now)
        assertEquals(
            CoreInboundDisposition.CARRIED,
            adapter.process("AA:BB:CC:DD:EE:FF", envelope, me),
        )
        assertEquals(1, intents.familyCarries)
    }

    @Test
    fun aRelaySourcedForeignRowIsNeverNudgedBackTowardTheRelay() {
        val me = generateIdentity()
        val sender = generateIdentity()
        val friendOfMine = generateIdentity()
        val store = MessageStore.open(":memory:")
        store.upsertImportedContact(contactOf(friendOfMine, "Friend"))
        val now = System.currentTimeMillis()
        val intents = RecordingIntents()
        val adapter = CoreInboundAdapter(store, SeenIds(), intents) { now }

        // Already durable on the relay; re-uploading it is exactly what the
        // relay-sourced carry class exists to prevent.
        val envelope = sealedEnvelopeTo(sender, friendOfMine, friendOfMine.userId, now)
        assertEquals(CoreInboundDisposition.CARRIED, adapter.process(null, envelope, me))
        assertEquals(0, intents.familyCarries)
    }

    @Test
    fun aDeliveredPayloadIsCommittedOnlyAfterTheShellPersistsIt() {
        val me = generateIdentity()
        val sender = generateIdentity()
        val store = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        val seen = SeenIds()
        val intents = RecordingIntents(deliverySucceeds = false)
        val adapter = CoreInboundAdapter(store, seen, intents) { now }

        val envelope = sealedEnvelopeTo(sender, me, me.userId, now)

        // DTN D4: a durable delivery that failed must leave the envelope
        // re-presentable and must never be acked.
        assertEquals(
            CoreInboundDisposition.FAILED,
            adapter.process("AA:BB:CC:DD:EE:FF", envelope, me),
        )
        assertEquals(1, intents.delivered.size)
        assertNull("a pairwise delivery is not a group delivery", intents.deliveredGroupIds.single())
        assertFalse(
            "a failed delivery must not poison the dedupe set",
            seen.contains(envelope.msgId),
        )

        // The same copy, presented again once the store is healthy, lands.
        intents.deliverySucceeds = true
        assertEquals(
            CoreInboundDisposition.CONSUMED,
            adapter.process("AA:BB:CC:DD:EE:FF", envelope, me),
        )
        assertEquals(2, intents.delivered.size)
        assertTrue(
            "a delivered payload is recorded seen by the post-delivery commit",
            seen.contains(envelope.msgId),
        )

        // And the third copy dedupes without touching the shell again.
        assertEquals(
            CoreInboundDisposition.SEEN,
            adapter.process("AA:BB:CC:DD:EE:FF", envelope, me),
        )
        assertEquals(2, intents.delivered.size)
    }

    @Test
    fun aGroupDeliveryNamesItsGroupSoTheShellNeverGuessesTheLane() {
        val me = generateIdentity()
        val member = generateIdentity()
        val group = createGroup("Family", listOf(me.userId, member.userId))
        val store = MessageStore.open(":memory:")
        store.upsertGroup(group)
        val now = System.currentTimeMillis()
        val intents = RecordingIntents()
        val adapter = CoreInboundAdapter(store, SeenIds(), intents) { now }

        val envelope = Frame.Envelope(
            msgId = generateMsgId(),
            hopTtl = 7u,
            expiry = now + 60_000,
            recipientHint = computeRecipientHint(group.id, now),
            sealed = sealGroupMessage(
                member,
                group,
                encodeMessageBody(
                    MessageBody(
                        kind = KIND_TEXT,
                        chatId = group.id,
                        lamport = 1u,
                        timestamp = now,
                        content = "hello group".toByteArray(),
                    ),
                ),
            ),
        )

        assertEquals(
            CoreInboundDisposition.CONSUMED,
            adapter.process("AA:BB:CC:DD:EE:FF", envelope, me),
        )
        val lane = intents.deliveredGroupIds.single()
        assertNotNull("core states the delivery lane", lane)
        assertTrue("and it is this group", lane!!.contentEquals(group.id))
        // A group body is still muled onward for members who were not here.
        assertEquals(1, intents.flooded.size)
    }

    // ---- Half two: ported disposition tests, on the core engine ---------

    private fun coreEngineProcessor(
        store: MessageStore,
        identity: Identity,
        requestRelaySync: (String) -> Unit = {},
    ): InboundEnvelopeProcessor {
        val context = FakeContext()
        InboundEngineSettings.setInboundEngine(context, InboundEngine.CORE)
        assertEquals(
            "the flag under test is actually on",
            InboundEngine.CORE,
            InboundEngineSettings.inboundEngine(context),
        )
        return InboundEnvelopeProcessor(
            context = context,
            store = store,
            identityProvider = { identity },
            requestRelaySync = requestRelaySync,
            lan = lanHooks,
        )
    }

    private fun friendRequestEnvelope(
        senderStore: MessageStore,
        sender: Identity,
        recipient: Identity,
        now: Long,
    ): RelayFetchedEnvelope {
        val recipientContact = senderStore.upsertImportedContact(contactOf(recipient, "Recipient"))
        val authored = senderStore.authorFriendRequest(
            sender,
            recipientContact,
            makeFriendCard("Mallory", sender, null, null),
            now,
        )
        return RelayFetchedEnvelope(
            id = authored.message.lamport.toLong(),
            msgId = authored.envelope.msgId,
            hopTtl = authored.envelope.hopTtl,
            recipientHint = authored.envelope.recipientHint,
            sealed = authored.envelope.sealed,
            expiryMs = authored.envelope.expiry,
        )
    }

    /** Ported from `BlockedSenderTest`, asserted through the core engine. */
    @Test
    fun blockedSenderFriendRequestIsDroppedAndUnblockRestores() {
        val mallory = generateIdentity()
        val dana = generateIdentity()
        val malloryStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        val processor = coreEngineProcessor(danaStore, dana)

        danaStore.blockUser(mallory.userId, now)
        runCatching {
            processor.handleRelayEnvelope(friendRequestEnvelope(malloryStore, mallory, dana, now), dana)
        }
        assertNull(
            "blocked sender's friend request must not create a contact",
            danaStore.getContact(mallory.userId),
        )

        danaStore.unblockUser(mallory.userId)
        runCatching {
            processor.handleRelayEnvelope(
                friendRequestEnvelope(malloryStore, mallory, dana, now + 1),
                dana,
            )
        }
        assertNotNull(
            "unblocked sender's fresh friend request should create the contact",
            danaStore.getContact(mallory.userId),
        )
    }

    private fun groupEnvelope(
        sender: Identity,
        group: uniffi.cruisemesh_core.Group,
        lamport: ULong,
        now: Long,
    ): RelayFetchedEnvelope {
        val body = encodeMessageBody(
            MessageBody(
                kind = KIND_TEXT,
                chatId = group.id,
                lamport = lamport,
                timestamp = now,
                content = "hello group".toByteArray(),
            ),
        )
        return RelayFetchedEnvelope(
            id = lamport.toLong(),
            msgId = generateMsgId(),
            hopTtl = 7u,
            recipientHint = computeRecipientHint(group.id, now),
            sealed = sealGroupMessage(sender, group, body),
            expiryMs = now + 60_000,
        )
    }

    /** Ported from `GroupMembershipEnforcementTest`, asserted through the core engine. */
    @Test
    fun nonMemberAndBlockedGroupEnvelopesAreDroppedMemberEnvelopeLands() {
        val dana = generateIdentity()
        val member = generateIdentity()
        val outsider = generateIdentity()
        val group = createGroup("Family", listOf(dana.userId, member.userId))
        val store = MessageStore.open(":memory:")
        store.upsertGroup(group)
        val now = System.currentTimeMillis()
        val processor = coreEngineProcessor(store, dana)

        runCatching {
            processor.handleRelayEnvelope(groupEnvelope(outsider, group, 1u, now), dana)
        }
        assertTrue(
            "non-member group envelope must not be stored",
            store.messagesForChat(group.id).isEmpty(),
        )

        store.blockUser(member.userId, now)
        runCatching {
            processor.handleRelayEnvelope(groupEnvelope(member, group, 1u, now), dana)
        }
        assertTrue(
            "blocked member group envelope must not be stored",
            store.messagesForChat(group.id).isEmpty(),
        )

        store.unblockUser(member.userId)
        runCatching {
            processor.handleRelayEnvelope(groupEnvelope(member, group, 2u, now), dana)
        }
        val messages = store.messagesForChat(group.id)
        assertEquals("member group envelope should land", 1, messages.size)
        assertTrue(messages[0].senderUserId.contentEquals(member.userId))
    }
}
