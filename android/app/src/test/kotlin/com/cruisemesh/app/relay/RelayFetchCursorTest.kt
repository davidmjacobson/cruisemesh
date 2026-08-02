package com.cruisemesh.app.relay

import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.relayBuildFetchPath
import uniffi.cruisemesh_core.relayCursorAdvance
import uniffi.cruisemesh_core.relayCursorKey
import uniffi.cruisemesh_core.relayFetchBatchLimit
import uniffi.cruisemesh_core.relayFetchShrunkLimit
import uniffi.cruisemesh_core.relayFetchWalkContinues
import uniffi.cruisemesh_core.relayPassStartCursor
import uniffi.cruisemesh_core.relaySweepDue
import uniffi.cruisemesh_core.relaySweepIntervalMs
import java.util.Base64

/**
 * The frontier that stopped every relay sync pass from re-walking the whole
 * mailbox from id 0.
 *
 * The bug these pin: a relay mailbox legitimately keeps rows nobody will ever
 * ack (a proxy-fetched copy stays as the durable fallback; a legacy group-hint
 * row is never acked at all), relayd returns rows in ascending id order, and a
 * *fresh* message therefore has the highest id and is fetched last. Restarting
 * at 0 every pass meant paging through everything stale before reaching
 * anything new -- minutes of delivery latency on a real mailbox, and passes
 * that timed out before finishing.
 *
 * Policy lives in the core (`core/src/relay_cursor.rs`) so both shells answer
 * identically; this exercises it through the binding the shell actually calls,
 * plus the two shell-side pieces (`pushSubscribePath` and the store round
 * trip).
 */
class RelayFetchCursorTest {

    private val url = "https://relay.example"
    private val token = "member-token"
    private fun key() = relayCursorKey(url, token)

    // -- mailbox identity ------------------------------------------------

    @Test
    fun `a mailbox key is stable across url spellings and carries no credential`() {
        assertEquals(key(), relayCursorKey("relay.example/", "  member-token  "))
        assertFalse(key().contains(token))
        assertFalse(key().contains("relay.example"))
    }

    @Test
    fun `rotating the token names a different mailbox so the cursor starts over`() {
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 9_000L, true)

        val rotated = relayCursorKey(url, "rotated-token")
        assertNotEquals(key(), rotated)
        assertEquals(0L, store.relayFetchCursor(rotated).afterId)
        assertEquals(0L, store.relayFetchCursor(rotated).lastSweepAtMs)
        // ...and the old mailbox is untouched by the rotation.
        assertEquals(9_000L, store.relayFetchCursor(key()).afterId)
    }

    @Test
    fun `two families on one host do not share a cursor`() {
        assertNotEquals(relayCursorKey(url, "family-one"), relayCursorKey(url, "family-two"))
    }

    // -- advance / do-not-advance policy ---------------------------------

    @Test
    fun `a fully processed page advances the persisted frontier`() {
        val store = MessageStore.open(":memory:")
        assertEquals(256L, store.advanceRelayFetchCursor(key(), 256L, true))
        assertEquals(512L, store.advanceRelayFetchCursor(key(), 512L, true))
        assertEquals(512L, store.relayFetchCursor(key()).afterId)
    }

    @Test
    fun `a page that failed mid-way never moves the frontier past it`() {
        // The mirror of the DTN ack-safety invariant: an envelope whose
        // processing threw must be re-presented next pass, which can only
        // happen if nothing was persisted past it.
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 256L, true)
        assertEquals(256L, store.advanceRelayFetchCursor(key(), 512L, false))
        assertEquals(256L, store.relayFetchCursor(key()).afterId)
        // The policy function says the same thing without a store.
        assertEquals(256L, relayCursorAdvance(256L, 512L, false))
        assertEquals(512L, relayCursorAdvance(256L, 512L, true))
    }

    @Test
    fun `a sweep re-reading old pages never rewinds the frontier`() {
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 9_000L, true)
        for (pageCursor in listOf(256L, 512L, 8_000L)) {
            store.advanceRelayFetchCursor(key(), pageCursor, true)
        }
        assertEquals(9_000L, store.relayFetchCursor(key()).afterId)
        store.advanceRelayFetchCursor(key(), 9_500L, true)
        assertEquals(9_500L, store.relayFetchCursor(key()).afterId)
    }

    @Test
    fun `an endpoint with no url or token persists nothing`() {
        val store = MessageStore.open(":memory:")
        assertEquals("", relayCursorKey(url, "   "))
        assertEquals(0L, store.advanceRelayFetchCursor("", 9_000L, true))
        store.noteRelaySweepCompleted("", 5_000L)
        assertEquals(0L, store.relayFetchCursor("").afterId)
        assertEquals(0L, store.relayFetchCursor("").lastSweepAtMs)
    }

    // -- sweep scheduling ------------------------------------------------

    @Test
    fun `the first pass of a process sweeps whatever the stored timestamp says`() {
        // Cold start is the self-healing answer to a frontier that has gone
        // stale in a way no response reveals -- most importantly a relay
        // rebuilt with its row ids restarted at 1.
        assertTrue(relaySweepDue(false, 0L, 1_000L))
        assertTrue(relaySweepDue(false, 1_000L, 1_000L))
        assertTrue(relaySweepDue(false, Long.MAX_VALUE, 1_000L))
    }

    @Test
    fun `later passes sweep only once the interval has elapsed`() {
        val sweptAt = 1_000_000L
        val interval = relaySweepIntervalMs()
        assertEquals(6L * 60 * 60 * 1000, interval)
        assertFalse(relaySweepDue(true, sweptAt, sweptAt))
        assertFalse(relaySweepDue(true, sweptAt, sweptAt + interval - 1))
        assertTrue(relaySweepDue(true, sweptAt, sweptAt + interval))
    }

    @Test
    fun `a backwards clock sweeps rather than pinning the mailbox`() {
        assertTrue(relaySweepDue(true, 5_000_000L, 1_000L))
        assertTrue(relaySweepDue(true, 0L, 5_000L))
    }

    @Test
    fun `a completed sweep restarts the interval without costing the frontier`() {
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 9_000L, true)
        store.noteRelaySweepCompleted(key(), 1_000_000L)
        val cursor = store.relayFetchCursor(key())
        assertEquals(9_000L, cursor.afterId)
        assertEquals(1_000_000L, cursor.lastSweepAtMs)
        assertFalse(relaySweepDue(true, cursor.lastSweepAtMs, 1_000_001L))
    }

    @Test
    fun `a sweep starts at zero and a normal pass resumes from the frontier`() {
        assertEquals(0L, relayPassStartCursor(true, 9_000L))
        assertEquals(9_000L, relayPassStartCursor(false, 9_000L))
        assertEquals(0L, relayPassStartCursor(false, -5L))
    }

    @Test
    fun `clearing every cursor makes the next pass re-walk`() {
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 9_000L, true)
        store.clearRelayFetchCursors()
        assertEquals(0L, store.relayFetchCursor(key()).afterId)
    }

    // -- batch limit and walk termination --------------------------------

    @Test
    fun `the fetch batch limit is the raised one and the path builder accepts it`() {
        val limit = relayFetchBatchLimit()
        assertEquals(256u, limit)
        // relayd's own MAX_FETCH_LIMIT is 500, so the deployed server takes
        // this without clamping.
        assertTrue(limit.toInt() <= 500)
        val path = relayBuildFetchPath(listOf(ByteArray(8) { 2 }), 0L, limit)
        assertTrue(path.contains("limit=256"))
    }

    @Test
    fun `a server that clamps the limit does not end the walk early`() {
        // We ask for 256 and a server hands back 50. Treating a short page as
        // end-of-mailbox would strand every row above it -- in an ascending-id
        // mailbox, all the new mail.
        assertTrue(relayFetchWalkContinues(50u, 0L, 50L))
        assertTrue(relayFetchWalkContinues(1u, 0L, 1L))
    }

    @Test
    fun `only an empty page ends the walk`() {
        assertFalse(relayFetchWalkContinues(0u, 100L, 100L))
        assertTrue(relayFetchWalkContinues(256u, 100L, 356L))
    }

    @Test
    fun `a page truncated by the server's byte budget keeps the walk going`() {
        // relayd stops filling a page once its cumulative sealed bytes would
        // push the response past what this client will decode, so a mailbox
        // of large attachment chunks answers a 256-row ask with a handful of
        // rows every time. Reading that as end-of-mailbox would strand the
        // newest mail, which in an ascending-id mailbox has the highest ids.
        assertTrue(relayFetchWalkContinues(12u, 0L, 12L))
        assertTrue(relayFetchWalkContinues(9u, 12L, 21L))
        assertTrue(relayFetchWalkContinues(1u, 21L, 22L))
        assertFalse(relayFetchWalkContinues(0u, 22L, 22L))
    }

    @Test
    fun `an oversize page halves the ask down to one row and then stops`() {
        var limit = relayFetchBatchLimit()
        val ladder = mutableListOf(limit)
        while (true) {
            val next = relayFetchShrunkLimit(limit) ?: break
            limit = next
            ladder += limit
        }
        assertEquals(listOf(256u, 128u, 64u, 32u, 16u, 8u, 4u, 2u, 1u), ladder)
        // One row is the floor: nothing smaller exists to ask for.
        assertNull(relayFetchShrunkLimit(1u))
    }

    @Test
    fun `a cursor that does not advance ends the walk instead of looping`() {
        assertFalse(relayFetchWalkContinues(16u, 100L, 100L))
        assertFalse(relayFetchWalkContinues(16u, 100L, 99L))
    }

    @Test
    fun `a real fetch asks for the frontier rather than starting over`() {
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 28_800L, true)
        val server = MockWebServer()
        server.enqueue(MockResponse().setResponseCode(200).setBody("""{"envelopes":[],"next_cursor":28800}"""))
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), token)
            val after = store.relayFetchCursor(key()).afterId
            RelayClient.fetchEnvelopes(
                config,
                listOf(ByteArray(8) { 2 }),
                after,
                relayFetchBatchLimit().toInt(),
            )
            val request = server.takeRequest()
            assertTrue(request.path!!.contains("after=28800"))
            assertTrue(request.path!!.contains("limit=256"))
        } finally {
            server.shutdown()
        }
    }

    // -- the doorbell subscribes from the frontier too --------------------

    @Test
    fun `the push subscribe path carries the frontier instead of a hardcoded zero`() {
        // relayd replays from `after` on every reconnect. At 0 that is the
        // entire mailbox, serialized into frames this client discards one by
        // one -- pure server load and bandwidth for no behavior change.
        val hint = ByteArray(8) { 2 }
        val encoded = Base64.getUrlEncoder().withoutPadding().encodeToString(hint)
        val path = pushSubscribePath(listOf(hint), 28_800L)
        assertEquals("/ws?hints=$encoded&after=28800", path)
    }

    @Test
    fun `a negative push cursor is clamped rather than rejected by the relay`() {
        val hint = ByteArray(8) { 2 }
        assertTrue(pushSubscribePath(listOf(hint), -1L).endsWith("&after=0"))
    }

    @Test
    fun `every subscribed hint reaches the push path`() {
        val hints = listOf(ByteArray(8) { 1 }, ByteArray(8) { 2 })
        val path = pushSubscribePath(hints, 0L)
        for (hint in hints) {
            assertTrue(path.contains(Base64.getUrlEncoder().withoutPadding().encodeToString(hint)))
        }
    }
}
