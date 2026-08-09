package com.cruisemesh.app.mesh

import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.relay.RelayPassEngine
import com.cruisemesh.app.relay.relayShadowPermitted
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayShadowLane
import uniffi.cruisemesh_core.CoreRelayShadowSampler
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreRelayShadowMaxRows
import java.net.HttpURLConnection
import java.net.Socket
import java.net.URI
import java.net.URL

/**
 * The canary's safety properties, checked as properties rather than trusted as
 * intentions.
 *
 * Several of these tests are structural: they assert the *shape* of what the
 * shadow is made of, because "it performs no network I/O" and "it writes
 * nothing operational" are claims that a future edit could quietly break while
 * every behavioural test kept passing.
 */
class RelayShadowAdapterTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }

        private const val NOW = 1_700_000_000_000L
        private const val URL_A = "https://relay.example"
        private const val TOKEN_A = "member-token"
        private const val URL_B = "https://other.example"
        private const val TOKEN_B = "other-token"
        private val RECIPIENT = ByteArray(32) { 0x09 }
    }

    // -----------------------------------------------------------------------
    // Structural: no second request and no second write are expressible
    // -----------------------------------------------------------------------

    @Test
    fun `nothing the capture holds could open a connection`() {
        // The capture is what the shadow is handed. If every value in it is a
        // number, a byte array, a string or an enum, then there is no object
        // in the shadow's reach that has a network method to call. The one
        // function handle it carries is the sampling arm, which answers a
        // boolean; it is named so the exception is deliberate rather than a
        // hole in the rule.
        val allowed = setOf(
            "java.util.List",
            "java.util.Map",
            "int",
            "boolean",
            "java.lang.Boolean",
        )
        val forbidden = listOf(
            Socket::class.java,
            URL::class.java,
            URI::class.java,
            HttpURLConnection::class.java,
        )
        for (field in RelayShadowPassCapture::class.java.declaredFields) {
            val type = field.type
            // Kotlin's `companion object` holder is not state the capture
            // carries; it is where the row cap it reads from core lives.
            if (type.isSynthetic || java.lang.reflect.Modifier.isStatic(field.modifiers)) continue
            if (field.name == "armSample") continue
            assertTrue(
                "RelayShadowPassCapture.${field.name} is a ${type.name}; the capture may hold only values",
                type.name in allowed || type.isPrimitive,
            )
            assertFalse(
                "RelayShadowPassCapture.${field.name} must not be a networking type",
                forbidden.any { it.isAssignableFrom(type) },
            )
        }
    }

    @Test
    fun `the adapter cannot reach a network type or a production write`() {
        val forbidden = listOf(Socket::class.java, URL::class.java, HttpURLConnection::class.java)
        for (field in RelayShadowAdapter::class.java.declaredFields) {
            if (field.type.isSynthetic) continue
            assertFalse(
                "RelayShadowAdapter.${field.name} must not be a networking type",
                forbidden.any { it.isAssignableFrom(field.type) },
            )
        }
        // The point of the sink. `MessageStore` is the one object carrying
        // every marker, cursor and health writer in the app as a public
        // method, so an adapter holding one has all of them a single line
        // away and only review keeps the second writer out. An adapter that
        // cannot name the type cannot call them.
        assertFalse(
            "the adapter must not hold the message store; it gets one bounded sink",
            RelayShadowAdapter::class.java.declaredFields.any {
                it.type == MessageStore::class.java
            },
        )
        assertTrue(
            "the adapter's one collaborator is the diagnostics sink",
            RelayShadowAdapter::class.java.declaredFields.any {
                it.type == RelayShadowReportSink::class.java
            },
        )
    }

    @Test
    fun `shadowing is refused when the core engine is the one running the pass`() {
        assertTrue(relayShadowPermitted(RelayPassEngine.LEGACY, shadowEnabled = true))
        assertFalse(relayShadowPermitted(RelayPassEngine.LEGACY, shadowEnabled = false))
        // Comparing the core planner against the core engine agrees every
        // time, which is indistinguishable from evidence and is not evidence.
        assertFalse(relayShadowPermitted(RelayPassEngine.CORE, shadowEnabled = true))
        assertFalse(relayShadowPermitted(RelayPassEngine.CORE, shadowEnabled = false))
    }

    // -----------------------------------------------------------------------
    // Behavioural
    // -----------------------------------------------------------------------

    @Test
    fun `the core engine gets no capture at all`() {
        val adapter = adapterFor(MessageStore.open(":memory:"), engine = RelayPassEngine.CORE)
        assertNull(adapter.beginPass(NOW))
    }

    @Test
    fun `with the canary off a legacy pass is untouched`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store, shadowEnabled = false)
        repeat(50) { step -> assertNull(adapter.beginPass(NOW + step * 3_600_000L)) }
        adapter.finishPass(null, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)
        assertFalse(store.exportProtocolEventsJsonl().contains("shadow"))
    }

    @Test
    fun `with the canary on and nothing to compare a legacy pass is still untouched`() {
        // The shipping default, which is canary *on*. A poll tick with an
        // empty outbound and receipt queue is the common relay pass, and it
        // must cost nothing: no record, and no sample spent, so the day's
        // budget stays available for a pass that carries rows.
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        repeat(50) { step ->
            val capture = adapter.beginPass(NOW + step * 3_600_000L)
            // A pass may still report the recipients it excluded; on its own
            // that is not evidence worth a sample.
            capture?.noteSkippedRecipients(listOf(RECIPIENT))
            adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)
        }
        assertFalse(store.exportProtocolEventsJsonl().contains("shadow"))
    }

    @Test
    fun `sampling is bounded rather than every pass`() {
        val adapter = adapterFor(MessageStore.open(":memory:"))
        assertTrue("the first pass with a row is sampled", sampledWithRow(adapter, NOW))
        // A burst -- a push frame, a queue change and a poll tick inside a
        // second -- must cost one sample, not three.
        assertFalse(sampledWithRow(adapter, NOW + 100))
        assertFalse(sampledWithRow(adapter, NOW + 200))
    }

    @Test
    fun `the daily bound survives a service restart`() {
        // The sampler lives outside the adapter for exactly this: a bound held
        // only in memory resets on every process launch, and a fresh sampler
        // always samples its first pass, so a phone whose foreground service
        // keeps being killed would sample nearly every pass.
        var persisted = CoreRelayShadowSampler(0L, 0u, 0L)
        var sampled = 0
        // A whole UTC day of passes, fifteen minutes apart, starting at
        // midnight so the calendar-day bound is the only one that can bite.
        val midnight = NOW / 86_400_000L * 86_400_000L
        repeat(95) { step ->
            // A new adapter each time: this *is* the restarted service.
            val adapter = RelayShadowAdapter(
                sink = { _, _ -> },
                passEngine = { RelayPassEngine.LEGACY },
                shadowEnabled = { true },
                loadSampler = { persisted },
                saveSampler = { persisted = it },
            )
            if (sampledWithRow(adapter, midnight + step * 900_000L)) sampled++
        }
        assertTrue("a restarting service must not sample every pass, got $sampled", sampled <= 12)
    }

    @Test
    fun `an agreeing pass records that it ran and finds nothing`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        val capture = adapter.beginPass(NOW)!!
        capture.noteSucceeded(
            CoreRelayShadowLane.RECEIPT, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A),
        )
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)

        val archive = store.exportProtocolEventsJsonl()
        assertTrue(archive.contains("\"outcome\":\"shadow_agreed\""))
        assertFalse(archive.contains(TOKEN_A))
        assertFalse(archive.contains(URL_A))
    }

    @Test
    fun `a mailbox fault only one engine keeps spending on is reported`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        val capture = adapter.beginPass(NOW)!!
        // A full mailbox is evidence about the mailbox, so core stops
        // spending on it; the legacy engine here offered it the next row
        // anyway, and that throughput difference is invisible in a status
        // code.
        capture.noteFailed(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A),
            RelayHttpException(507, "mailbox_full", "full"),
        )
        capture.noteSucceeded(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A),
        )
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)

        val archive = store.exportProtocolEventsJsonl()
        assertTrue(archive.contains("\"outcome\":\"shadow_diverged\""))
        // The summary record plus one record per kind of disagreement. Which
        // token each kind is recorded under is core's to state and core's
        // tests assert it; restating the table here would be a second place
        // the stable names are written down.
        assertTrue(
            "a divergent sample must record the summary and each finding",
            archive.split("\"code\":\"shadow_mismatch\"").size - 1 >= 2,
        )
    }

    @Test
    fun `a failure with no next row for that mailbox did not continue the lane`() {
        // The axis is "did the pass go on to offer this mailbox the next row
        // of this lane", and the only honest answer is the observed one. A
        // single queued row that fails continued nothing, however recoverable
        // the status code looks.
        val adapter = adapterFor(MessageStore.open(":memory:"))
        val capture = adapter.beginPass(NOW)!!
        capture.noteFailed(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A), RelayHttpException(500, null, "boom"),
        )
        // A row for a *different* mailbox is not this mailbox continuing.
        capture.noteSucceeded(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_B, TOKEN_B),
        )
        assertFalse(capture.steps()[0].legacyContinuedLane)
    }

    @Test
    fun `a failure the pass followed with another row to the same mailbox did continue the lane`() {
        val adapter = adapterFor(MessageStore.open(":memory:"))
        val capture = adapter.beginPass(NOW)!!
        capture.noteFailed(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A), RelayHttpException(413, null, "too big"),
        )
        capture.noteSucceeded(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A),
        )
        assertTrue(capture.steps()[0].legacyContinuedLane)
    }

    @Test
    fun `a comparison writes only diagnostics, never the queue`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        val capture = adapter.beginPass(NOW)!!
        capture.noteDeclined(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
        )
        capture.noteSkippedRecipients(listOf(RECIPIENT))
        capture.noteUnshadowed(2)
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)

        // A store the canary has just run against still holds no message, no
        // envelope and no cursor of its own making.
        assertEquals(0, store.pendingRelayOutboundEnvelopes(64uL, NOW, emptyList()).size)
        assertEquals(0, store.listContactRelayRejections().size)
        assertEquals(0, store.listContactRelayUnreachable().size)
        assertEquals(0L, store.relayFetchCursor("any").afterId)
        val archive = store.exportProtocolEventsJsonl()
        assertTrue(archive.contains("\"rows_unshadowed\":2"))
    }

    @Test
    fun `a canary that throws never becomes the pass's failure`() {
        // `finishPass` is called from a `finally`, so anything it throws
        // replaces the exception that was unwinding the pass -- a family rate
        // limit would surface as a plain failure and the retry window would go
        // unlogged.
        val adapter = RelayShadowAdapter(
            sink = { _, _ -> throw IllegalStateException("the store is closed") },
            passEngine = { RelayPassEngine.LEGACY },
            shadowEnabled = { true },
            loadSampler = { CoreRelayShadowSampler(0L, 0u, 0L) },
            saveSampler = { },
        )
        val capture = adapter.beginPass(NOW)!!
        capture.noteSucceeded(
            CoreRelayShadowLane.RECEIPT, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
            RelayConfig(URL_A, TOKEN_A),
        )
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)
    }

    @Test
    fun `a capture is bounded and says how much it dropped`() {
        val adapter = adapterFor(MessageStore.open(":memory:"))
        val capture = adapter.beginPass(NOW)!!
        val cap = coreRelayShadowMaxRows().toInt()
        repeat(cap + 5) {
            capture.noteSucceeded(
                CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, NOW,
                RelayConfig(URL_A, TOKEN_A),
            )
        }
        assertEquals(cap, capture.steps().size)
        assertEquals(5, capture.rowsDropped())
        // Dropped rows are unshadowed rows: a report over sixteen of twenty-one
        // must not read as a report over twenty-one.
        assertEquals(5u, capture.rowsUnshadowed())
    }

    /** Runs one pass that has a row worth comparing, and says whether it was sampled. */
    private fun sampledWithRow(adapter: RelayShadowAdapter, nowMs: Long): Boolean {
        val capture = adapter.beginPass(nowMs) ?: return false
        capture.noteSucceeded(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, SEALED_LEN, nowMs,
            RelayConfig(URL_A, TOKEN_A),
        )
        val sampled = capture.steps().isNotEmpty()
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), nowMs)
        return sampled
    }

    private fun adapterFor(
        store: MessageStore,
        engine: RelayPassEngine = RelayPassEngine.LEGACY,
        shadowEnabled: Boolean = true,
    ): RelayShadowAdapter {
        var state = CoreRelayShadowSampler(0L, 0u, 0L)
        return RelayShadowAdapter(
            sink = store::noteRelayShadowReport,
            passEngine = { engine },
            shadowEnabled = { shadowEnabled },
            loadSampler = { state },
            saveSampler = { state = it },
        )
    }

    private fun contacts(usable: Boolean) = listOf(
        CoreRelayShadowContact(RECIPIENT, null, null, usable),
    )

    private fun msgId() = ByteArray(16) { 0x11 }
    private fun hint() = ByteArray(8) { 0x22 }
}

private const val SEALED_LEN = 48
