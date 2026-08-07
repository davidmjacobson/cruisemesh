package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.coreGroupFanoutRows
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.sealGroupMessage
import java.io.File

/**
 * Pins the receive leg of group relay fan-out
 * (specs/group-relay-durability.md §4.3, field-found broken 2026-07-24): a
 * per-member fan-out row is addressed to the MEMBER's own hint, not the
 * group's, so the pre-fix hint-only group-open gate misfiled every fetched
 * copy as foreign mule traffic (CARRIED, never delivered, never acked).
 * With [uniffi.cruisemesh_core.MessageStore.groupOpenCandidates] the copy
 * must open, land in the group chat, and disposition CONSUMED so the relay
 * row is acked away. Runs the real [InboundEnvelopeProcessor] against an
 * inert JVM Context.
 */
class GroupFanoutRelayDeliveryTest {

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
        private val files = File.createTempFile("fanout", null).parentFile!!

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

    @Test
    fun fetchedFanoutRowDeliversAsGroupMessageAndConsumes() {
        val dana = generateIdentity()
        val author = generateIdentity()
        val group = createGroup("Family", listOf(author.userId, dana.userId))
        val store = MessageStore.open(":memory:")
        store.upsertGroup(group)
        val now = System.currentTimeMillis()

        // Sender side, exactly as the upload pass builds it: one group-sealed
        // body, decomposed into per-member rows addressed to each member's
        // OWN hint (spec §4.1/§4.2).
        val body = encodeMessageBody(
            MessageBody(
                kind = KIND_TEXT,
                chatId = group.id,
                lamport = 1u,
                timestamp = now,
                content = "dinner at 6:30 on deck 8".toByteArray(),
            ),
        )
        val rows = coreGroupFanoutRows(
            originalMsgId = generateMsgId(),
            memberUserIds = group.memberUserIds,
            hopTtl = 7u,
            expiry = now + 60_000,
            sealed = sealGroupMessage(author, group, body),
            envelopeTimestampMs = now,
        )
        val danaHint = computeRecipientHint(dana.userId, now)
        val danaRow = rows.single { it.recipientHint.contentEquals(danaHint) }

        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = store,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
        )
        // On-screen chat: skips MessageNotifier, whose Base64 call NPEs on
        // the JVM (same reason sibling tests runCatching the delivery); the
        // production notification path is not what this test pins.
        ChatVisibility.setVisible(group.id)
        val disposition = try {
            processor.handleRelayEnvelope(
                RelayFetchedEnvelope(
                    id = 1,
                    msgId = danaRow.msgId,
                    hopTtl = danaRow.hopTtl,
                    recipientHint = danaRow.recipientHint,
                    sealed = danaRow.sealed,
                    expiryMs = danaRow.expiry,
                ),
                dana,
            )
        } finally {
            ChatVisibility.reset()
        }

        // CONSUMED (not the pre-fix CARRIED) is what lets the relay sync
        // pass ack dana's row away instead of refetching it until expiry.
        assertEquals(CoreInboundDisposition.CONSUMED, disposition)
        val messages = store.messagesForChat(group.id)
        assertEquals("fan-out copy must land in the group chat", 1, messages.size)
        assertTrue(messages[0].senderUserId.contentEquals(author.userId))
    }
}
