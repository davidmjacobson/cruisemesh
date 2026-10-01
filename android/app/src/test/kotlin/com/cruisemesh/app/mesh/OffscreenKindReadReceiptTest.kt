package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.notify.ChatVisibility
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.ProfileSyncContent
import uniffi.cruisemesh_core.encodeProfileSyncContent
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.makeFriendCard
import java.io.File

/**
 * A read receipt claims the person saw the content, so only a row the
 * conversation actually renders may ever raise the READ watermark --
 * `coreIsVisibleChatKind` is the one list that says which those are, and
 * neither a friend request (kind=3) nor a profile sync (kind=5) is on it.
 *
 * Keying READ off "this chat is on screen" for those two was wrong twice
 * over. The chat can only be on screen when an existing contact re-sends (a
 * stranger has no chat to open), and the watermark a receipt reports is a
 * plain MAX over what this device holds -- so an off-screen kind that
 * overtook a still-in-flight text turned that text's tick blue on the
 * sender's phone before this one had received it. iOS never did this; these
 * tests pin the two shells to the same answer.
 *
 * The DELIVERED half is asserted alongside every READ assertion on purpose:
 * it is what retires the sender's carried copy, and it must keep advancing.
 */
class OffscreenKindReadReceiptTest {

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
        private val files = File.createTempFile("offscreen-kinds", null).parentFile!!

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

    private fun envelopeFrame(envelope: OutboundEnvelope) = Frame.Envelope(
        msgId = envelope.msgId,
        hopTtl = envelope.hopTtl,
        expiry = envelope.expiry,
        recipientHint = envelope.recipientHint,
        sealed = envelope.sealed,
    )

    /**
     * Alice and Bob already hold each other, which is the only shape in which
     * Bob can have Alice's chat open when one of her off-screen kinds lands.
     */
    private class Peers(val alice: Identity, val bob: Identity) {
        val aliceStore: MessageStore = MessageStore.open(":memory:")
        val bobStore: MessageStore = MessageStore.open(":memory:")
        val bobHeldByAlice: Contact

        init {
            bobHeldByAlice = aliceStore.upsertImportedContact(
                Contact(
                    userId = bob.userId,
                    name = "Bob",
                    signPk = bob.signPk,
                    agreePk = bob.agreePk,
                    relayUrl = null,
                    relayToken = null,
                ),
            )
            bobStore.upsertContact(
                Contact(
                    userId = alice.userId,
                    name = "Alice",
                    signPk = alice.signPk,
                    agreePk = alice.agreePk,
                    relayUrl = null,
                    relayToken = null,
                ),
            )
        }
    }

    private fun processorFor(store: MessageStore, identity: Identity) = InboundEnvelopeProcessor(
        context = FakeContext(),
        store = store,
        identityProvider = { identity },
        requestRelaySync = {},
        lan = lanHooks,
    )

    /** Delivers [envelope] to Bob with Alice's chat registered as on screen. */
    private fun receiveWithChatOnScreen(peers: Peers, envelope: OutboundEnvelope): CoreInboundDisposition {
        val processor = processorFor(peers.bobStore, peers.bob)
        ChatVisibility.setVisible(peers.alice.userId)
        return try {
            processor.processInboundEnvelope("aa:bb", envelopeFrame(envelope), peers.bob)
        } finally {
            ChatVisibility.reset()
        }
    }

    private fun outgoing(peers: Peers, receiptType: UByte): ULong =
        peers.bobStore.outgoingReceiptThrough(peers.alice.userId, peers.alice.userId, receiptType)

    @Test
    fun friendRequestArrivingOnTheOpenChatIsDeliveredButNeverRead() {
        val peers = Peers(generateIdentity(), generateIdentity())
        val authored = peers.aliceStore.authorFriendRequest(
            peers.alice,
            peers.bobHeldByAlice,
            makeFriendCard("Alice", peers.alice, null, null),
            System.currentTimeMillis(),
        )

        assertEquals(
            CoreInboundDisposition.CONSUMED,
            receiveWithChatOnScreen(peers, authored.envelope),
        )
        assertEquals(
            "a consumed friend request must still move the sender's delivered watermark",
            authored.message.lamport,
            outgoing(peers, RECEIPT_TYPE_DELIVERED),
        )
        assertEquals(
            "a friend request is never rendered, so it may never claim a read",
            0uL,
            outgoing(peers, RECEIPT_TYPE_READ),
        )
    }

    @Test
    fun profileSyncArrivingOnTheOpenChatIsDeliveredButNeverRead() {
        val peers = Peers(generateIdentity(), generateIdentity())
        val authored = peers.aliceStore.authorPairwiseMessage(
            peers.alice,
            peers.bobHeldByAlice,
            KIND_PROFILE_SYNC,
            encodeProfileSyncContent(
                ProfileSyncContent(
                    avatarEpoch = 1L,
                    name = "Alice",
                    avatar = ByteArray(0),
                    friendsOfFriendsVersion = 1u,
                    friendsOfFriendsEnabled = false,
                    friendsOfFriendsRevision = 0uL,
                ),
            ),
            null,
            System.currentTimeMillis(),
        )

        assertEquals(
            CoreInboundDisposition.CONSUMED,
            receiveWithChatOnScreen(peers, authored.envelope),
        )
        assertEquals(
            authored.message.lamport,
            outgoing(peers, RECEIPT_TYPE_DELIVERED),
        )
        assertEquals(
            "a profile sync is never rendered, so it may never claim a read",
            0uL,
            outgoing(peers, RECEIPT_TYPE_READ),
        )
    }

    @Test
    fun textArrivingOnTheOpenChatIsStillRead() {
        // The other side of the rule, so this never over-corrects into
        // withholding the read tick people actually rely on.
        val peers = Peers(generateIdentity(), generateIdentity())
        val authored = peers.aliceStore.authorPairwiseMessage(
            peers.alice,
            peers.bobHeldByAlice,
            KIND_TEXT,
            "we are at the buffet".toByteArray(),
            null,
            System.currentTimeMillis(),
        )

        assertEquals(
            CoreInboundDisposition.CONSUMED,
            receiveWithChatOnScreen(peers, authored.envelope),
        )
        assertEquals(authored.message.lamport, outgoing(peers, RECEIPT_TYPE_DELIVERED))
        assertEquals(
            "a text read on an open chat still earns its read tick",
            authored.message.lamport,
            outgoing(peers, RECEIPT_TYPE_READ),
        )
    }
}
