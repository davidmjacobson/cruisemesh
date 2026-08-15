package com.cruisemesh.app.mesh

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.IncomingMessageAnnouncer
import com.cruisemesh.app.relay.CoreRelayDriver
import com.cruisemesh.app.relay.normalizeRelayUrl
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreRelayEndpointConfig
import uniffi.cruisemesh_core.CoreRelayPassOutcome
import uniffi.cruisemesh_core.CoreRelayPassPlan
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.coreRelayPassDefaultBudgets
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.recentPresenceHintsFor
import java.io.File
import java.util.Base64

/**
 * What a core relay pass hands back to the shell, and what the shell does with
 * it.
 *
 * The core pass was complete long before this test existed, and a device
 * flipped to it still went quiet: pages were fetched, rows were persisted, acks
 * were sent -- and nobody was told a message had arrived, and no contact's
 * "last seen" moved. Both were shell gaps rather than core ones, which is
 * exactly why they were invisible from the core's own test suite.
 *
 * So these are end-to-end on purpose. A real [MessageStore], the real
 * `CoreRelayPass`, the real [CoreRelayPassRunner] and [CoreRelayDriver], a real
 * socket, and on the other end the real [InboundEnvelopeProcessor] with the
 * same recording announcer the notification release gate uses. The claim being
 * pinned is not "the projection callback fires" -- it is "a message fetched by
 * the core engine reaches the same notification a message fetched by the legacy
 * engine reaches".
 */
class CoreRelayPassProjectionTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }

        /**
         * This device's clock, near real time on purpose: the inbound path
         * this projection feeds drops envelopes past their expiry against the
         * *real* clock, so a fixed timestamp from years ago would make every
         * delivery in this file expire before it was opened.
         */
        private val NOW = System.currentTimeMillis()
    }

    // -----------------------------------------------------------------------
    // Gap 1: delivered, and no longer silent
    // -----------------------------------------------------------------------

    @Test
    fun `a message the core pass ingests raises the notification the legacy walk raises`() {
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            relay.fetchPages += fixture.pageOf(fixture.textFromPeer("we're at the buffet on deck 9"), 7)

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.COMPLETED, summary.outcome)
            assertEquals("core must have persisted the row itself", 1u, summary.rowsIngested)
            assertEquals(
                "the message must be stored under the sender's chat",
                1,
                fixture.messagesFromPeer(),
            )
            assertEquals(
                "the person must be told, exactly as on the legacy engine",
                listOf("we're at the buffet on deck 9"),
                fixture.announcer.direct,
            )
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a re-presented message the pass has already ingested announces nothing further`() {
        // The mailbox re-offers rows this device deliberately never acked, and
        // a sweep walks the whole mailbox from zero. A second notification per
        // message is the failure on the other side of the release gate: the
        // person learns to distrust the badge. Core reports only the rows its
        // ingest transaction newly took, so the second sighting projects
        // nothing at all.
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            relay.fetchPages += fixture.pageOf(fixture.textFromPeer("meet at the gangway"), 7)

            fixture.run()

            // The same envelope again, higher up the mailbox: a row this
            // device deliberately never acked, offered a second time.
            relay.fetchPages += fixture.pageOf(fixture.lastSent(), 9)
            fixture.run()

            assertEquals("stored once", 1, fixture.messagesFromPeer())
            assertEquals("announced once, not twice", 1, fixture.announcer.direct.size)
        } finally {
            relay.shutdown()
        }
    }

    // -----------------------------------------------------------------------
    // Gap 2: presence, projected
    // -----------------------------------------------------------------------

    @Test
    fun `a presence answer moves the contact's last seen`() {
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            relay.presenceBody = fixture.presencePageForPeer(ageMs = 5_000L)

            fixture.run()

            val seen = MeshConnectivityStatus.presenceLastSeen.value[fixture.peerHex()]
            assertEquals(
                "the contact's last seen must move to the answer's age, on this device's clock",
                NOW - 5_000L,
                seen,
            )
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a relay with nothing to say about a contact leaves their last seen alone`() {
        // An empty answer is a real answer, and it is not evidence of absence:
        // it must not invent a sighting, and it must not erase one either.
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            relay.presenceBody = """{"now_ms":$NOW,"presence":[]}"""

            fixture.run()

            assertFalse(
                "no answer must not become a sighting",
                MeshConnectivityStatus.presenceLastSeen.value.containsKey(fixture.peerHex()),
            )
        } finally {
            relay.shutdown()
        }
    }

    // -----------------------------------------------------------------------
    // Gap 3: two brakes, not one
    // -----------------------------------------------------------------------

    @Test
    fun `a rested endpoint is reported silent rather than unusable`() {
        // The regression this pins is a one-character one: folding the two
        // answers into `endpointUsable` reads a contact whose host has merely
        // gone quiet as a contact whose host refused us, and core then posts
        // their mail into this device's own mailbox instead of leaving it
        // queued.
        val silent = contactNamed("Silent")
        val rejected = contactNamed("Rejected")

        val configs = coreRelayContactConfigs(
            listOf(silent, rejected),
            endpointUsable = { it.name != "Rejected" },
            endpointAnswering = { it.name != "Silent" },
        )

        val silentConfig = configs.single { it.userId.contentEquals(silent.userId) }
        assertTrue("silence is not rejection evidence", silentConfig.endpointUsable)
        assertFalse("a quiet endpoint must be reported as not answering", silentConfig.endpointAnswering)

        val rejectedConfig = configs.single { it.userId.contentEquals(rejected.userId) }
        assertFalse("a refusal must be reported as not usable", rejectedConfig.endpointUsable)
        assertTrue("a refusal is not silence evidence", rejectedConfig.endpointAnswering)
    }

    @Test
    fun `mail for a silent endpoint stays queued instead of being posted to our own mailbox`() {
        // The payoff of the split, end to end through the real pass: silence
        // declines the upload, while a rejection takes the fallback. With both
        // brakes folded into one flag the first case behaved like the second,
        // and the marker that misroute writes is terminal.
        val relay = FakeRelay()
        relay.start()
        try {
            val silent = Fixture(relay.baseUrl(), contactEndpointAnswering = false)
            silent.queueMessageToPeer()
            silent.run()
            assertEquals("a quiet host earns no post at all", 0, relay.posts)
            assertEquals("and the message stays queued for a later pass", 1, silent.pendingOutbound())

            relay.rewind()

            val refused = Fixture(relay.baseUrl(), contactEndpointUsable = false)
            refused.queueMessageToPeer()
            refused.run()
            assertEquals("a refusal falls back to our own mailbox", 1, relay.posts)
        } finally {
            relay.shutdown()
        }
    }

    // -----------------------------------------------------------------------
    // Harness
    // -----------------------------------------------------------------------

    @Before
    fun clearSurfaces() {
        ChatVisibility.reset()
        MeshConnectivityStatus.clear()
    }

    @After
    fun clearSurfacesAfter() {
        ChatVisibility.reset()
        MeshConnectivityStatus.clear()
    }

    private fun contactNamed(name: String): Contact {
        val who = generateIdentity()
        return Contact(
            userId = who.userId,
            name = name,
            signPk = who.signPk,
            agreePk = who.agreePk,
            relayUrl = null,
            relayToken = null,
        )
    }

    /**
     * This device, a contact, this device's own mailbox, and the whole shell
     * projection wired to the real inbound processor.
     */
    private class Fixture(
        private val baseUrl: String,
        private val contactEndpointUsable: Boolean = true,
        private val contactEndpointAnswering: Boolean = true,
    ) {
        private val identity = generateIdentity()
        private val peer = generateIdentity()
        private val store = MessageStore.open(":memory:")

        /** The contact's own store, used only to author what they send us. */
        private val peerStore = MessageStore.open(":memory:")
        val announcer = RecordingAnnouncer()

        private val contact = Contact(
            userId = peer.userId,
            name = "Peer",
            signPk = peer.signPk,
            agreePk = peer.agreePk,
            relayUrl = null,
            relayToken = null,
        )

        private val processor = InboundEnvelopeProcessor(
            context = FakeContext(),
            store = store,
            identityProvider = { identity },
            requestRelaySync = {},
            lan = NoLanHooks,
            announcer = announcer,
        )

        private val projector = CoreRelayPassProjector(
            deliver = { envelope, who -> processor.handleRelayEnvelope(envelope, who) },
            mergePresence = MeshConnectivityStatus::mergePresenceLastSeen,
        )

        init {
            store.upsertContact(contact)
            peerStore.upsertContact(
                Contact(
                    userId = identity.userId,
                    name = "Me",
                    signPk = identity.signPk,
                    agreePk = identity.agreePk,
                    relayUrl = null,
                    relayToken = null,
                ),
            )
        }

        fun peerHex(): String = UserIdHex.encode(peer.userId)

        fun messagesFromPeer(): Int = store.messagesForChat(peer.userId).size

        fun pendingOutbound(): Int =
            store.pendingRelayOutboundEnvelopes(64uL, NOW, emptyList()).size

        fun queueMessageToPeer() {
            store.authorPairwiseMessage(identity, contact, KIND_TEXT, "hello".toByteArray(), null, NOW)
        }

        /**
         * The sealed text the contact would have sent us, authored once so the
         * same envelope can be re-presented under a second relay row id.
         */
        private var sent: OutboundEnvelope? = null

        fun textFromPeer(text: String): OutboundEnvelope {
            val envelope = peerStore.authorPairwiseMessage(
                peer,
                Contact(
                    userId = identity.userId,
                    name = "Me",
                    signPk = identity.signPk,
                    agreePk = identity.agreePk,
                    relayUrl = null,
                    relayToken = null,
                ),
                KIND_TEXT,
                text.toByteArray(),
                null,
                NOW,
            ).envelope
            sent = envelope
            return envelope
        }

        /** One page carrying [envelope] as the mailbox row [id]. */
        fun pageOf(envelope: OutboundEnvelope, id: Long): String {
            val b64 = Base64.getUrlEncoder().withoutPadding()
            return """{"envelopes":[{"id":$id,"msg_id":"${b64.encodeToString(envelope.msgId)}",""" +
                """"hop_ttl":${envelope.hopTtl},""" +
                """"recipient_hint":"${b64.encodeToString(envelope.recipientHint)}",""" +
                """"sealed":"${b64.encodeToString(envelope.sealed)}",""" +
                """"expiry_ms":${envelope.expiry}}],"next_cursor":$id}"""
        }

        fun lastSent(): OutboundEnvelope = checkNotNull(sent)

        fun presencePageForPeer(ageMs: Long): String {
            val b64 = Base64.getUrlEncoder().withoutPadding()
            val hint = recentPresenceHintsFor(peer.userId, NOW).first()
            return """{"now_ms":$NOW,"presence":[{"hint":"${b64.encodeToString(hint)}",""" +
                """"last_seen_ms":${NOW - ageMs}}]}"""
        }

        fun run() = CoreRelayPassRunner(
            store = store,
            executor = { passId, actionId, request, atMs ->
                CoreRelayDriver.execute(passId, actionId, request, null, atMs)
            },
            clock = { NOW },
            onProjection = { projection ->
                projector.project(projection, identity, listOf(contact), NOW)
            },
        ).run(plan(), "t")

        private fun plan() = CoreRelayPassPlan(
            own = CoreRelayEndpointConfig(baseUrl, "member-token"),
            contacts = coreRelayContactConfigs(
                listOf(contact),
                endpointUsable = { contactEndpointUsable },
                endpointAnswering = { contactEndpointAnswering },
            ),
            ownUserId = identity.userId,
            fetchHints = store.relayFetchHints(identity.userId, NOW),
            presenceAnnounce = emptyList(),
            presenceQuery = recentPresenceHintsFor(peer.userId, NOW),
            ownEndpointChanged = false,
            sweptThisSession = true,
            consecutiveRateLimits = 0u,
            quietUntilMs = 0L,
            budgets = coreRelayPassDefaultBudgets(),
        )
    }

    /** Records what the delivery path asked the user to be told about. */
    private class RecordingAnnouncer : IncomingMessageAnnouncer {
        val direct = mutableListOf<String>()
        val group = mutableListOf<String>()

        override fun announceDirectMessage(contact: Contact, preview: String) {
            direct += preview
        }

        override fun announceGroupMessage(group: Group, senderName: String, preview: String) {
            this.group += "$senderName: $preview"
        }

        override fun announceGroupInvite(group: Group) {}
        override fun announceFriendAdded(contact: Contact) {}
    }

    private object NoLanHooks : InboundEnvelopeProcessor.LanHooks {
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
        private val files = File.createTempFile("relayprojection", null).parentFile!!

        override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
            prefs.getOrPut(name ?: "") { FakePrefs() }
        override fun getApplicationContext(): Context = this
        override fun getFilesDir(): File = files
    }

    /** A relay that answers, and counts what it was asked. */
    private class FakeRelay {
        private val server = MockWebServer()
        var posts = 0
            private set

        /**
         * Pages this mailbox will hand out, in order. A fetch past the end of
         * the queue answers with an empty mailbox, which is what ends a walk.
         */
        val fetchPages = ArrayDeque<String>()

        /** The presence answer; nobody seen by default. */
        var presenceBody = """{"now_ms":$NOW,"presence":[]}"""

        /** Forgets the counters, so a second pass can be measured on its own. */
        fun rewind() {
            posts = 0
        }

        fun start() {
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path.orEmpty()
                    if (path == "/envelopes" && request.method == "POST") {
                        posts++
                        return MockResponse().setResponseCode(200).setBody("""{"id":1}""")
                    }
                    if (path == "/presence") {
                        return MockResponse().setResponseCode(200).setBody(presenceBody)
                    }
                    if (path.startsWith("/envelopes?") && request.method == "GET") {
                        val body = fetchPages.removeFirstOrNull()
                            ?: """{"envelopes":[],"next_cursor":0}"""
                        return MockResponse().setResponseCode(200).setBody(body)
                    }
                    return MockResponse().setResponseCode(200).setBody("{}")
                }
            }
            server.start()
        }

        fun baseUrl(): String = normalizeRelayUrl(server.url("/").toString())

        fun shutdown() = server.shutdown()
    }
}
