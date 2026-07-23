package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.makeFriendCard
import java.io.File

/**
 * A blocked identity's envelopes are dropped before any kind handler runs: a
 * replayed kind=3 friend request must not (re)create the contact. After an
 * unblock, a fresh request lands normally. Runs the real
 * [InboundEnvelopeProcessor] receive path against an inert JVM Context.
 */
class BlockedSenderTest {

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
        private val files = File.createTempFile("blocked", null).parentFile!!

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
    }

    private fun friendRequestEnvelope(
        senderStore: MessageStore,
        sender: uniffi.cruisemesh_core.Identity,
        recipient: uniffi.cruisemesh_core.Identity,
        now: Long,
    ): RelayFetchedEnvelope {
        val recipientContact = senderStore.upsertImportedContact(
            Contact(
                userId = recipient.userId,
                name = "Recipient",
                signPk = recipient.signPk,
                agreePk = recipient.agreePk,
                relayUrl = null,
                relayToken = null,
            ),
        )
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

    @Test
    fun blockedSenderFriendRequestIsDroppedAndUnblockRestores() {
        val mallory = generateIdentity()
        val dana = generateIdentity()
        val malloryStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()

        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = danaStore,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
        )

        danaStore.blockUser(mallory.userId, now)
        runCatching {
            processor.handleRelayEnvelope(friendRequestEnvelope(malloryStore, mallory, dana, now), dana)
        }
        assertNull(
            "blocked sender's friend request must not create a contact",
            danaStore.getContact(mallory.userId),
        )

        danaStore.unblockUser(mallory.userId)
        // A fresh request (new msg_id — the first one is in the gossip
        // seen-set) lands normally once unblocked.
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
}
