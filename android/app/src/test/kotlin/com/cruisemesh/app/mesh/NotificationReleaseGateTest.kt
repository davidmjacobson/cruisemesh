package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.IncomingMessageAnnouncer
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.generateIdentity
import java.io.File

/**
 * The executable form of ROADMAP.md's notification release gate:
 *
 * > **Notification reliability as a release gate:** background delivery must
 * > produce a timely local notification on real devices (screen off, battery
 * > saver, hours idle) before the app is offered to anyone beyond the
 * > development family. The incumbent apps' single most common failure is
 * > "the message arrived and nobody knew" -- this project refuses to ship that.
 *
 * Audited 2026-07-30. The *code* half of that gate had zero coverage: the
 * notify branch was the one branch of [InboundEnvelopeProcessor] no JVM test
 * could reach, because [com.cruisemesh.app.notify.MessageNotifier] touches
 * `Context.getSystemService`/`Base64` and throws on the bare JVM. Three
 * sibling tests had grown workarounds *around* it rather than through it --
 * `GroupFanoutRelayDeliveryTest` marks the chat on-screen to skip it,
 * `BlockedSenderTest` and `GroupMembershipEnforcementTest` `runCatching` the
 * whole delivery. So nothing would have failed if the notify call were
 * deleted outright.
 *
 * With [IncomingMessageAnnouncer] injectable, this pins the invariant the
 * gate is actually about, on the real processor and the real core:
 *
 * **A newly stored, user-visible message whose chat is not on screen
 * announces itself exactly once. A duplicate announces zero more times. An
 * on-screen chat announces zero.**
 *
 * What this deliberately does NOT claim: the *device* half of the gate
 * (screen off, battery saver, hours idle, Doze). That needs two phones in
 * hand and cannot be asserted here -- the protocol for it lives in the scout
 * `FOR-DAVID.md`. This test proves the delivery path *asks* for a
 * notification; only a real device proves Android then shows it.
 */
class NotificationReleaseGateTest {

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
            l: SharedPreferences.OnSharedPreferenceChangeListener?,
        ) {}
        override fun unregisterOnSharedPreferenceChangeListener(
            l: SharedPreferences.OnSharedPreferenceChangeListener?,
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
        private val files = File.createTempFile("notifgate", null).parentFile!!

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

    /** Records what the delivery path asked the user to be told about. */
    private class RecordingAnnouncer : IncomingMessageAnnouncer {
        val direct = mutableListOf<String>()
        val group = mutableListOf<String>()
        val invites = mutableListOf<String>()
        val friends = mutableListOf<String>()

        override fun announceDirectMessage(contact: Contact, preview: String) {
            direct += preview
        }

        override fun announceGroupMessage(group: Group, senderName: String, preview: String) {
            this.group += "$senderName: $preview"
        }

        override fun announceGroupInvite(group: Group) {
            invites += group.name
        }

        override fun announceFriendAdded(contact: Contact) {
            friends += contact.name
        }

        fun total() = direct.size + group.size + invites.size + friends.size
    }

    @Before
    fun clearVisibility() = ChatVisibility.reset()

    @After
    fun clearVisibilityAfter() = ChatVisibility.reset()

    private fun contactFor(identity: Identity, name: String) = Contact(
        userId = identity.userId,
        name = name,
        signPk = identity.signPk,
        agreePk = identity.agreePk,
        relayUrl = null,
        relayToken = null,
    )

    /**
     * Builds the pairwise text envelope [sender] would author to [recipient],
     * shaped as a relay fetch so it can be fed through the same
     * [InboundEnvelopeProcessor.handleRelayEnvelope] entry point the relay
     * sync pass uses.
     */
    private fun textEnvelope(
        senderStore: MessageStore,
        sender: Identity,
        recipient: Identity,
        text: String,
        at: Long,
    ): RelayFetchedEnvelope {
        val authored = senderStore.authorPairwiseMessage(
            sender,
            contactFor(recipient, "Dana"),
            KIND_TEXT,
            text.toByteArray(),
            null,
            at,
        )
        return RelayFetchedEnvelope(
            id = at,
            msgId = authored.envelope.msgId,
            hopTtl = authored.envelope.hopTtl,
            recipientHint = authored.envelope.recipientHint,
            sealed = authored.envelope.sealed,
            expiryMs = authored.envelope.expiry,
        )
    }

    @Test
    fun `a direct message arriving with its chat off screen announces exactly once`() {
        val katie = generateIdentity()
        val dana = generateIdentity()
        val katieStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        danaStore.upsertContact(contactFor(katie, "Katie"))

        val announcer = RecordingAnnouncer()
        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = danaStore,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
            announcer = announcer,
        )

        // No ChatVisibility.setVisible: the app is backgrounded, or looking at
        // some other screen. This is the release gate's exact scenario, and it
        // is the call that used to throw instead of being observable.
        processor.handleRelayEnvelope(
            textEnvelope(katieStore, katie, dana, "we're at the buffet on deck 9", now),
            dana,
        )

        assertEquals("the message must be stored", 1, danaStore.messagesForChat(katie.userId).size)
        assertEquals("exactly one notification for one message", 1, announcer.direct.size)
        assertEquals("we're at the buffet on deck 9", announcer.direct.single())
        assertEquals("nothing else should have been announced", 1, announcer.total())
    }

    @Test
    fun `a duplicate of an already-announced message announces nothing further`() {
        // Digest sync re-offers envelopes the peer cannot prove we have
        // (DESIGN.md 7.3). A second notification for one message is the
        // failure mode on the other side of the gate -- the user learns to
        // distrust the badge, which is how "nobody knew" starts.
        val katie = generateIdentity()
        val dana = generateIdentity()
        val katieStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        danaStore.upsertContact(contactFor(katie, "Katie"))

        val announcer = RecordingAnnouncer()
        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = danaStore,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
            announcer = announcer,
        )
        val envelope = textEnvelope(katieStore, katie, dana, "meet at the gangway", now)

        processor.handleRelayEnvelope(envelope, dana)
        processor.handleRelayEnvelope(envelope, dana)

        assertEquals("stored once", 1, danaStore.messagesForChat(katie.userId).size)
        assertEquals("announced once, not twice", 1, announcer.direct.size)
    }

    @Test
    fun `a message for the chat already on screen announces nothing`() {
        val katie = generateIdentity()
        val dana = generateIdentity()
        val katieStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        danaStore.upsertContact(contactFor(katie, "Katie"))

        val announcer = RecordingAnnouncer()
        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = danaStore,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
            announcer = announcer,
        )

        ChatVisibility.setVisible(katie.userId)
        processor.handleRelayEnvelope(
            textEnvelope(katieStore, katie, dana, "look at the fjord", now),
            dana,
        )

        assertEquals("still stored", 1, danaStore.messagesForChat(katie.userId).size)
        assertEquals("no notification for the chat being read", 0, announcer.total())
    }

    @Test
    fun `a message for a different chat still announces while another chat is on screen`() {
        // The suppression key is per-chat. Reading Katie's chat must not
        // silence Caleb's -- a whole-app mute triggered by any open chat would
        // be exactly the incumbent bug, and ChatVisibility holds a single
        // slot, so this is worth pinning rather than assuming.
        val katie = generateIdentity()
        val caleb = generateIdentity()
        val dana = generateIdentity()
        val calebStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        danaStore.upsertContact(contactFor(katie, "Katie"))
        danaStore.upsertContact(contactFor(caleb, "Caleb"))

        val announcer = RecordingAnnouncer()
        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = danaStore,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
            announcer = announcer,
        )

        ChatVisibility.setVisible(katie.userId)
        processor.handleRelayEnvelope(
            textEnvelope(calebStore, caleb, dana, "can I have the wifi code", now),
            dana,
        )

        assertEquals("Caleb's message must still announce", 1, announcer.direct.size)
    }

    @Test
    fun `a group message arriving with the group off screen announces exactly once`() {
        val author = generateIdentity()
        val dana = generateIdentity()
        val group = createGroup("Family", listOf(author.userId, dana.userId))
        val authorStore = MessageStore.open(":memory:")
        val danaStore = MessageStore.open(":memory:")
        val now = System.currentTimeMillis()
        authorStore.upsertGroup(group)
        danaStore.upsertGroup(group)
        danaStore.upsertContact(contactFor(author, "Katie"))

        val announcer = RecordingAnnouncer()
        val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = danaStore,
            identityProvider = { dana },
            requestRelaySync = {},
            lan = lanHooks,
            announcer = announcer,
        )

        val authored = authorStore.authorGroupMessage(
            author,
            group,
            KIND_TEXT,
            "dinner at 6:30 on deck 8".toByteArray(),
            null,
            now,
        )
        processor.handleRelayEnvelope(
            RelayFetchedEnvelope(
                id = 1,
                msgId = authored.envelope.msgId,
                hopTtl = authored.envelope.hopTtl,
                recipientHint = authored.envelope.recipientHint,
                sealed = authored.envelope.sealed,
                expiryMs = authored.envelope.expiry,
            ),
            dana,
        )

        assertEquals("group message must be stored", 1, danaStore.messagesForChat(group.id).size)
        assertEquals("exactly one group notification", 1, announcer.group.size)
        assertTrue(
            "the group notification should name the sender: ${announcer.group.single()}",
            announcer.group.single().contains("dinner at 6:30 on deck 8"),
        )
    }
}
