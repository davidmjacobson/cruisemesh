package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.sealGroupMessage
import java.io.File

/**
 * Pins the receive-side group membership guard (deliverOpenedGroupEnvelope):
 * `openGroupMessage` deliberately does not check the signer's membership (see
 * its core doc comment), so an envelope sealed by a NON-member with a leaked
 * group key opens fine — the routing layer must drop it. A member-sealed
 * envelope with the same shape must land. Runs the real
 * [InboundEnvelopeProcessor] against an inert JVM Context.
 */
class GroupMembershipEnforcementTest {

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
        private val files = File.createTempFile("groupguard", null).parentFile!!

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

    @Test
    fun nonMemberGroupEnvelopeIsDroppedMemberEnvelopeLands() {
        val dana = generateIdentity()
        val member = generateIdentity()
        val outsider = generateIdentity()
        val group = createGroup("Family", listOf(dana.userId, member.userId))
        val store = MessageStore.open(":memory:")
        store.upsertGroup(group)
        val now = System.currentTimeMillis()

        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = store,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
        )

        // Outsider holds the group key (leak/removed-member scenario) and can
        // seal an envelope that decrypts and signature-verifies — the routing
        // guard must still drop it.
        runCatching {
            processor.handleRelayEnvelope(groupEnvelope(outsider, group, 1u, now), dana)
        }
        assertTrue(
            "non-member group envelope must not be stored",
            store.messagesForChat(group.id).isEmpty(),
        )

        runCatching {
            processor.handleRelayEnvelope(groupEnvelope(member, group, 1u, now), dana)
        }
        val messages = store.messagesForChat(group.id)
        assertEquals("member group envelope should land", 1, messages.size)
        assertTrue(messages[0].senderUserId.contentEquals(member.userId))
    }
}
