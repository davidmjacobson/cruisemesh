package com.cruisemesh.app.mesh

import com.cruisemesh.app.relay.CoreRelayDriver
import com.cruisemesh.app.relay.normalizeRelayUrl
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreRelayEndpointConfig
import uniffi.cruisemesh_core.CoreRelayPassOutcome
import uniffi.cruisemesh_core.CoreRelayPassPlan
import uniffi.cruisemesh_core.CoreRelayTransportError
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.coreRelayPassDefaultBudgets
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.relayCursorKey

/**
 * A whole core relay pass, driven end to end by the code that will drive it on
 * a phone, against a relay that is real enough to be wrong.
 *
 * This is the test the migration actually rests on. The vector suite proves
 * the bytes of one request; the driver suite proves one result is reported
 * correctly. Neither proves that `CoreRelayPass`, [CoreRelayPassRunner] and
 * [CoreRelayDriver] compose into something that terminates, posts what is
 * queued, retires what the relay accepted, and leaves the queue alone when it
 * does not -- which is the only claim that would let anyone turn the flag on.
 *
 * Nothing here is a fake seam except the relay itself. The store is a real
 * `MessageStore`, the pass is the real session, the requests go over a real
 * socket through [CoreRelayDriver].
 */
class CoreRelayPassRunnerTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }

        private const val NOW = 1_700_000_000_000L
    }

    @Test
    fun `a full pass posts the queue, retires what the relay took, and finishes`() {
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            fixture.queueAuthored(3)

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.COMPLETED, summary.outcome)
            assertEquals(3u, summary.authoredUploads)
            assertEquals(3, relay.posts)
            // Retired, so the next pass offers what is behind them instead of
            // these three forever.
            assertEquals(0, fixture.pendingAuthored())
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a relay that refuses the mailbox leaves every row queued`() {
        val relay = FakeRelay(postResponse = { MockResponse().setResponseCode(507).setBody("""{"code":"mailbox_full"}""") })
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            fixture.queueAuthored(3)

            val summary = fixture.run()

            assertEquals(0u, summary.authoredUploads)
            assertEquals(3, fixture.pendingAuthored())
            // The lane stopped spending on a mailbox that said it was full,
            // rather than offering it every remaining row.
            assertEquals(1, relay.posts)
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `the first family rate limit ends the pass and reports the window it earned`() {
        val relay = FakeRelay(
            postResponse = {
                MockResponse().setResponseCode(429)
                    .setHeader("Retry-After", "30")
                    .setBody("""{"code":"rate_limited"}""")
            },
        )
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl())
            fixture.queueAuthored(4)

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.RATE_LIMITED, summary.outcome)
            assertEquals(1, relay.posts)
            assertTrue(
                "the quiet window must be at least the advertised 30s",
                summary.quietUntilMs >= NOW + 30_000,
            )
            // Nothing was retired: a refusal is not a delivery, and the row
            // the relay said no to is still the first one the next pass offers.
            assertEquals(4, fixture.pendingAuthored())
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a cancelled pass stops asking and admits it`() {
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl(), cancelled = true)
            fixture.queueAuthored(2)

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.CANCELLED, summary.outcome)
            assertEquals(0, relay.posts)
            assertEquals(2, fixture.pendingAuthored())
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a pass started inside a quiet window spends nothing`() {
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl(), quietUntilMs = NOW + 60_000)
            fixture.queueAuthored(2)

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.REFUSED_QUIET_WINDOW, summary.outcome)
            assertEquals(0, relay.posts)
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a relay that cannot be reached at all costs the queue nothing`() {
        // No server: the port is closed, so every request is a transport
        // failure and none of them may retire a row.
        val fixture = Fixture(normalizeRelayUrl("http://127.0.0.1:1"))
        fixture.queueAuthored(2)

        val summary = fixture.run()

        assertEquals(0u, summary.authoredUploads)
        assertEquals(2, fixture.pendingAuthored())
        assertTrue(
            "a pass that reached nothing must not report itself healthy",
            summary.outcome != CoreRelayPassOutcome.COMPLETED || summary.requestsIssued > 0u,
        )
    }

    // -----------------------------------------------------------------------
    // The walk: the lanes an upload-only pass never reaches
    // -----------------------------------------------------------------------

    @Test
    fun `a walk fetches a page over a real socket, acks what it consumed, and moves the frontier`() {
        // Everything the upload lane cannot exercise: a GET carrying a query
        // string the driver must not mangle, a decoded page, the ack POST that
        // follows it, and the cursor the pair earns.
        val relay = FakeRelay()
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl(), hinted = true)
            val page = fixture.consumedPage(listOf(3L, 5L, 8L))
            relay.fetchBody = page

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.COMPLETED, summary.outcome)
            assertTrue("the walk must have fetched, saw ${relay.fetchPaths}", relay.fetchPaths.isNotEmpty())
            val first = relay.fetchPaths.first()
            assertTrue("a fetch must carry its hints and cursor, got $first", first.startsWith("/envelopes?hints="))
            assertTrue("a fetch must carry its cursor, got $first", first.contains("&after=0&limit="))
            assertEquals("the page's rows must earn exactly one ack", 1, relay.acks)
            assertTrue("the ack must name the ids the page carried", relay.ackBody.contains("[3,5,8]"))
            assertEquals(3u, summary.rowsAcked)
            // CURSOR-01: the frontier moves once the ack it was waiting on
            // succeeded, and not before.
            assertEquals(8L, fixture.frontier())
        } finally {
            relay.shutdown()
        }
    }

    @Test
    fun `a rate limit on a fetch is read from the header that fetch answered with`() {
        // The `Retry-After` selection on a GET, which no post-only pass
        // reaches: the driver must hand back the one header core asked for
        // from a non-2xx answer, or `RATE-01` measures a window from nothing.
        val relay = FakeRelay()
        relay.fetchResponse = {
            MockResponse().setResponseCode(429)
                .setHeader("Retry-After", "45")
                .setBody("""{"code":"rate_limited"}""")
        }
        relay.start()
        try {
            val fixture = Fixture(relay.baseUrl(), hinted = true)

            val summary = fixture.run()

            assertEquals(CoreRelayPassOutcome.RATE_LIMITED, summary.outcome)
            assertTrue(
                "the quiet window must be at least the advertised 45s, got ${summary.quietUntilMs - NOW}",
                summary.quietUntilMs >= NOW + 45_000,
            )
        } finally {
            relay.shutdown()
        }
    }

    // -----------------------------------------------------------------------
    // Harness
    // -----------------------------------------------------------------------

    /** A store with an identity, a contact and this device's own pass. */
    private class Fixture(
        private val baseUrl: String,
        private val cancelled: Boolean = false,
        private val quietUntilMs: Long = 0L,
        private val hinted: Boolean = false,
    ) {
        private val identity = generateIdentity()
        private val peer = generateIdentity()
        private val store = MessageStore.open(":memory:")
        private val contact = Contact(
            userId = peer.userId,
            name = "Peer",
            signPk = peer.signPk,
            agreePk = peer.agreePk,
            relayUrl = null,
            relayToken = null,
        )

        init {
            store.upsertContact(contact)
        }

        private fun ownHint(): ByteArray = computeRecipientHint(identity.userId, NOW)

        /**
         * A relay page of rows this device has already durably consumed.
         *
         * Consumed rather than fresh because `ACK-01` forbids acking a carried
         * copy: a row nobody here consumed is one this device is only muling,
         * and acking it would delete another person's mail. A re-presented
         * consumed row is the one shape that legitimately earns an ack.
         */
        fun consumedPage(ids: List<Long>): String {
            val b64 = java.util.Base64.getUrlEncoder().withoutPadding()
            val hint = ownHint()
            val expiry = NOW + 6 * 24 * 60 * 60 * 1000L
            val rows = ids.joinToString(",") { id ->
                val msgId = ByteArray(16).also {
                    it[0] = id.toByte()
                    it[8] = 0xA5.toByte()
                }
                val sealed = ByteArray(96) { id.toByte() }
                check(
                    store.coreRecordConsumedHiddenMsgId(msgId, KIND_RECEIPT, hint, expiry, identity.userId, NOW),
                ) { "the consumed set must vouch for the seeded row" }
                """{"id":$id,"msg_id":"${b64.encodeToString(msgId)}","hop_ttl":3,""" +
                    """"recipient_hint":"${b64.encodeToString(hint)}",""" +
                    """"sealed":"${b64.encodeToString(sealed)}","expiry_ms":$expiry}"""
            }
            return """{"envelopes":[$rows],"next_cursor":${ids.last()}}"""
        }

        fun frontier(): Long = store.relayFetchCursor(relayCursorKey(baseUrl, "member-token")).afterId

        fun queueAuthored(count: Int) {
            repeat(count) { index ->
                store.authorPairwiseMessage(
                    identity,
                    contact,
                    1u,
                    "row-$index".toByteArray(),
                    null,
                    NOW,
                )
            }
        }

        fun pendingAuthored(): Int =
            store.pendingRelayOutboundEnvelopes(64uL, NOW, emptyList()).size

        fun run() = CoreRelayPassRunner(
            store = store,
            executor = { passId, actionId, request, atMs ->
                CoreRelayDriver.execute(passId, actionId, request, null, atMs)
            },
            clock = { NOW },
            isCancelled = { cancelled },
        ).run(plan(), "t")

        private fun plan() = CoreRelayPassPlan(
            own = CoreRelayEndpointConfig(baseUrl, "member-token"),
            contacts = emptyList(),
            ownUserId = identity.userId,
            fetchHints = if (hinted) listOf(ownHint()) else emptyList(),
            presenceAnnounce = emptyList(),
            presenceQuery = emptyList(),
            ownEndpointChanged = false,
            sweptThisSession = true,
            consecutiveRateLimits = 0u,
            quietUntilMs = quietUntilMs,
            budgets = coreRelayPassDefaultBudgets(),
        )
    }

    /** A relay that answers, counts, and can be told to say no. */
    private class FakeRelay(
        private val postResponse: () -> MockResponse = {
            MockResponse().setResponseCode(200).setBody("""{"id":1}""")
        },
    ) {
        private val server = MockWebServer()
        var posts = 0
            private set
        var acks = 0
            private set
        var ackBody = ""
            private set
        val fetchPaths = mutableListOf<String>()

        /** The page a fetch answers with; an empty mailbox by default. */
        var fetchBody = """{"envelopes":[],"next_cursor":0}"""

        /** Overrides [fetchBody] entirely when a fetch must fail. */
        var fetchResponse: (() -> MockResponse)? = null

        fun start() {
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path.orEmpty()
                    if (path == "/envelopes" && request.method == "POST") {
                        posts++
                        return postResponse()
                    }
                    if (path == "/envelopes/ack" && request.method == "POST") {
                        acks++
                        ackBody = request.body.readUtf8()
                        return MockResponse().setResponseCode(200).setBody("{}")
                    }
                    if (path.startsWith("/envelopes?") && request.method == "GET") {
                        fetchPaths += path
                        return fetchResponse?.invoke()
                            ?: MockResponse().setResponseCode(200).setBody(fetchBody)
                    }
                    return MockResponse().setResponseCode(200).setBody("{}")
                }
            }
            server.start()
        }

        fun baseUrl(): String = normalizeRelayUrl(server.url("/").toString())

        fun shutdown() = server.shutdown()
    }

    @Test
    fun `the runner never lets a session spin`() {
        // A driver result that names a pass the session does not recognise is
        // ignored by IDEMP-01, which means the session re-states its action.
        // Left alone that is an infinite loop between two correct components,
        // and the guard is what turns it into a bounded failure.
        val fixture = Fixture(normalizeRelayUrl("http://127.0.0.1:1"))
        fixture.queueAuthored(1)
        var issued = 0
        val summary = CoreRelayPassRunner(
            store = MessageStore.open(":memory:"),
            executor = { _, _, _, atMs ->
                issued++
                uniffi.cruisemesh_core.CoreRelayHttpResult(
                    passId = "not-this-pass",
                    actionId = 999uL,
                    status = 0u,
                    headers = emptyList(),
                    body = ByteArray(0),
                    error = CoreRelayTransportError.OTHER,
                    completedAtMs = atMs,
                )
            },
            clock = { NOW },
        ).run(
            CoreRelayPassPlan(
                own = CoreRelayEndpointConfig(normalizeRelayUrl("http://127.0.0.1:1"), "t"),
                contacts = emptyList(),
                ownUserId = ByteArray(32) { 1 },
                fetchHints = listOf(ByteArray(8) { 2 }),
                presenceAnnounce = emptyList(),
                presenceQuery = emptyList(),
                ownEndpointChanged = false,
                sweptThisSession = true,
                consecutiveRateLimits = 0u,
                quietUntilMs = 0L,
                budgets = coreRelayPassDefaultBudgets(),
            ),
            "g",
        )
        assertEquals(CoreRelayPassOutcome.CANCELLED, summary.outcome)
        assertTrue("the guard must bound the loop, saw $issued", issued in 1..1_000)
    }
}
