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
 * Three of these tests are structural: they assert the *shape* of what the
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
        private val RECIPIENT = ByteArray(32) { 0x09 }
    }

    // -----------------------------------------------------------------------
    // Structural: no second request is expressible
    // -----------------------------------------------------------------------

    @Test
    fun `nothing the capture holds could open a connection`() {
        // The capture is what the shadow is handed. If every value in it is a
        // number, a byte array, a string or an enum, then there is no object
        // in the shadow's reach that has a network method to call and no
        // function handle it could invoke to get one. That is what makes "no
        // duplicate external I/O" structural instead of aspirational.
        val allowed = setOf(
            "java.util.List",
            "int",
            "boolean",
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
    fun `nothing the adapter is constructed from could open a connection`() {
        val forbidden = listOf(Socket::class.java, URL::class.java, HttpURLConnection::class.java)
        for (field in RelayShadowAdapter::class.java.declaredFields) {
            if (field.type.isSynthetic) continue
            assertFalse(
                "RelayShadowAdapter.${field.name} must not be a networking type",
                forbidden.any { it.isAssignableFrom(field.type) },
            )
        }
        // The only collaborator it has that can touch anything at all.
        assertTrue(
            "the adapter must hold the store it records into",
            RelayShadowAdapter::class.java.declaredFields.any {
                it.type == MessageStore::class.java
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
        val adapter = RelayShadowAdapter(
            store = MessageStore.open(":memory:"),
            passEngine = { RelayPassEngine.CORE },
            shadowEnabled = { true },
        )
        assertNull(adapter.beginPass(NOW))
    }

    @Test
    fun `with the canary off a legacy pass is untouched`() {
        // The default path's safety property, stated as a test: engine legacy
        // plus canary off means beginPass answers null every time, so every
        // recorder the upload loops call is a no-op on a null reference and
        // the store never hears from the shadow at all.
        val store = MessageStore.open(":memory:")
        val adapter = RelayShadowAdapter(
            store = store,
            passEngine = { RelayPassEngine.LEGACY },
            shadowEnabled = { false },
        )
        repeat(50) { step -> assertNull(adapter.beginPass(NOW + step * 3_600_000L)) }
        adapter.finishPass(null, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)
        assertFalse(store.exportProtocolEventsJsonl().contains("shadow"))
    }

    @Test
    fun `sampling is bounded rather than every pass`() {
        val adapter = RelayShadowAdapter(
            store = MessageStore.open(":memory:"),
            passEngine = { RelayPassEngine.LEGACY },
            shadowEnabled = { true },
        )
        assertNotNull("the first pass of a day is sampled", adapter.beginPass(NOW))
        // A burst -- a push frame, a queue change and a poll tick inside a
        // second -- must cost one sample, not three.
        assertNull(adapter.beginPass(NOW + 100))
        assertNull(adapter.beginPass(NOW + 200))
    }

    @Test
    fun `an agreeing pass records that it ran and finds nothing`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        val capture = adapter.beginPass(NOW)!!
        capture.noteSucceeded(
            CoreRelayShadowLane.RECEIPT, msgId(), 4u, hint(), RECIPIENT, sealed(), NOW,
            RelayConfig(URL_A, TOKEN_A),
        )
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)

        val archive = store.exportProtocolEventsJsonl()
        assertTrue(archive.contains("\"outcome\":\"shadow_agreed\""))
        assertFalse(archive.contains(TOKEN_A))
        assertFalse(archive.contains(URL_A))
    }

    @Test
    fun `a row the legacy engine retired after a refusal is reported`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        val capture = adapter.beginPass(NOW)!!
        // The shape that loses mail: a 500, and the row retired anyway.
        capture.noteFailed(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, sealed(), NOW,
            RelayConfig(URL_A, TOKEN_A),
            RelayHttpException(500, null, "boom"),
            continuedLane = true,
        )
        adapter.finishPass(capture, RelayConfig(URL_A, TOKEN_A), contacts(usable = true), NOW)

        val archive = store.exportProtocolEventsJsonl()
        assertTrue(archive.contains("\"outcome\":\"shadow_diverged\""))
        // The summary record plus one record per disagreement. Which token
        // each kind is recorded under is core's to state and core's tests
        // assert it; restating the table here would be a second place the
        // stable names are written down.
        assertTrue(
            "a divergent sample must record the summary and each finding",
            archive.split("\"code\":\"shadow_mismatch\"").size - 1 >= 2,
        )
        assertTrue(archive.contains("\"mismatches\":1"))
    }

    @Test
    fun `a comparison writes only diagnostics, never the queue`() {
        val store = MessageStore.open(":memory:")
        val adapter = adapterFor(store)
        val capture = adapter.beginPass(NOW)!!
        capture.noteDeclined(
            CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, sealed(), NOW,
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
    fun `a capture is bounded and says how much it dropped`() {
        val adapter = adapterFor(MessageStore.open(":memory:"))
        val capture = adapter.beginPass(NOW)!!
        val cap = coreRelayShadowMaxRows().toInt()
        repeat(cap + 5) {
            capture.noteSucceeded(
                CoreRelayShadowLane.AUTHORED, msgId(), 4u, hint(), RECIPIENT, sealed(), NOW,
                RelayConfig(URL_A, TOKEN_A),
            )
        }
        assertEquals(cap, capture.steps().size)
        assertEquals(5, capture.rowsDropped())
        // Dropped rows are unshadowed rows: a report over sixteen of twenty-one
        // must not read as a report over twenty-one.
        assertEquals(5u, capture.rowsUnshadowed())
    }

    private fun adapterFor(store: MessageStore) = RelayShadowAdapter(
        store = store,
        passEngine = { RelayPassEngine.LEGACY },
        shadowEnabled = { true },
    )

    private fun contacts(usable: Boolean) = listOf(
        CoreRelayShadowContact(RECIPIENT, null, null, usable),
    )

    private fun msgId() = ByteArray(16) { 0x11 }
    private fun hint() = ByteArray(8) { 0x22 }
    private fun sealed() = ByteArray(48) { 0x33 }
}
