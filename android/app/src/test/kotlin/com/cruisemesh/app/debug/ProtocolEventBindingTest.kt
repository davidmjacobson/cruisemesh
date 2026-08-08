package com.cruisemesh.app.debug

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreSprayLanePlan
import uniffi.cruisemesh_core.CoreSprayPlanShape
import uniffi.cruisemesh_core.CoreSprayPolicy
import uniffi.cruisemesh_core.MessageStore

/**
 * The protocol-event ring across the UniFFI boundary.
 *
 * Deliberately not a second copy of the ring's rules -- Rust owns eviction,
 * redaction, the schema and the pseudonyms, and `core/tests` is where those
 * are proved. What this pins is the boundary itself, which Rust tests cannot
 * reach: that the export comes back as a whole string rather than a truncated
 * one, that the gating call returns a real boolean, and that handing the spray
 * policy a `MessageStore` -- the first place this project passes one core
 * object into another across the FFI -- actually connects the two.
 */
class ProtocolEventBindingTest {
    private fun store(): MessageStore = MessageStore.open(":memory:")

    @Test
    fun `an exported ring crosses the boundary as one whole JSONL document`() {
        val store = store()
        store.clearProtocolEvents()
        assertFalse("a cleared ring has nothing to send", store.hasProtocolEvents())

        // A relay page that moved the frontier: a real emit point, driven the
        // way the mailbox walker drives it.
        store.advanceRelayFetchCursor(MAILBOX_KEY, 100L, true)
        assertTrue(store.hasProtocolEvents())

        val jsonl = store.exportProtocolEventsJsonl()
        val lines = jsonl.trim().lines()
        assertTrue("expected a header and at least one record, got $jsonl", lines.size >= 2)
        assertTrue(lines.first().contains("\"record\":\"header\""))
        assertTrue(lines.first().contains("cruisemesh.protocol-event/v1"))
        assertTrue(lines.any { it.contains("\"code\":\"frontier_advanced\"") })
        assertTrue("the archive ends with a newline", jsonl.endsWith("\n"))

        // The token in the config key is exactly what must not survive the trip.
        assertFalse(jsonl.contains("cmdep1-"))
        assertFalse(jsonl.contains("://"))
    }

    @Test
    fun `attaching the store to the spray policy carries decisions into the ring`() {
        val store = store()
        store.clearProtocolEvents()
        val policy = CoreSprayPolicy()
        policy.attachEventJournal(store)

        val plan = CoreSprayPlanShape(
            carried = CoreSprayLanePlan(setDigest = 11uL, bytes = 4096uL),
            ownOutbound = CoreSprayLanePlan(setDigest = 22uL, bytes = 512uL),
            ownReceipts = CoreSprayLanePlan(setDigest = 0uL, bytes = 0uL),
        )
        policy.admitPlan(PEER_KEY, LINK_KEY, plan, NOW)
        // The same advertised set again inside the re-offer interval.
        policy.admitPlan(PEER_KEY, LINK_KEY, plan, NOW + 1_000L)

        val jsonl = store.exportProtocolEventsJsonl()
        assertTrue(jsonl.contains("spray_admitted"))
        assertTrue(jsonl.contains("spray_suppressed"))
        assertFalse("the raw peer key must not reach the archive", jsonl.contains(PEER_KEY))
    }

    @Test
    fun `clearing the ring is what delete captured diagnostics needs it to be`() {
        val store = store()
        store.advanceRelayFetchCursor(MAILBOX_KEY, 7L, true)
        assertTrue(store.hasProtocolEvents())
        store.clearProtocolEvents()
        assertFalse(store.hasProtocolEvents())
        assertEquals(
            "a cleared ring exports a header and nothing else",
            1,
            store.exportProtocolEventsJsonl().trim().lines().size,
        )
    }

    private companion object {
        const val MAILBOX_KEY = "https://relay.example.invalid/|cmdep1-secrettoken"
        const val PEER_KEY = "aabbccddeeff"
        const val LINK_KEY = "link-1"
        const val NOW = 1_700_000_000_000L
    }
}
