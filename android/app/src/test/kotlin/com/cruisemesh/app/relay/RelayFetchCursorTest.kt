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
import uniffi.cruisemesh_core.relayFrontierAfterCompletedSweep
import uniffi.cruisemesh_core.relayPassStartCursor
import uniffi.cruisemesh_core.relaySweepDue
import uniffi.cruisemesh_core.relaySweepIntervalMs
import uniffi.cruisemesh_core.relaySweepRestartFromZero
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
 * And the sweep's own resume cursor, which is the second half of the same
 * story: the walk is bounded per pass, a sweep is only recorded complete on
 * the empty page at the end of the mailbox, so a sweep that restarted at 0 on
 * every yield could never finish on a mailbox big enough to need the bound.
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
        store.noteRelaySweepCompleted("", 5_000L, 0L)
        assertEquals(0L, store.relayFetchCursor("").afterId)
        assertEquals(0L, store.relayFetchCursor("").lastSweepAtMs)
    }

    // -- sweep scheduling ------------------------------------------------

    @Test
    fun `a cold start honours the stored sweep timestamp instead of re-walking`() {
        // This service is killed and restarted all day (Doze, swipe-away,
        // memory pressure). A sweep re-downloads the sealed body of every row
        // still in the mailbox, so forcing one per process start made the
        // restart rate -- not the interval -- set the bandwidth bill.
        val sweptAt = 1_000_000L
        val interval = relaySweepIntervalMs()
        assertFalse(relaySweepDue(false, sweptAt, 0L, sweptAt))
        assertFalse(relaySweepDue(false, sweptAt, 0L, sweptAt + interval - 1))
        // Stale enough, and a cold start sweeps like any other pass.
        assertTrue(relaySweepDue(false, sweptAt, 0L, sweptAt + interval))
    }

    @Test
    fun `later passes sweep only once the interval has elapsed`() {
        val sweptAt = 1_000_000L
        val interval = relaySweepIntervalMs()
        assertEquals(6L * 60 * 60 * 1000, interval)
        assertFalse(relaySweepDue(true, sweptAt, 0L, sweptAt))
        assertFalse(relaySweepDue(true, sweptAt, 0L, sweptAt + interval - 1))
        assertTrue(relaySweepDue(true, sweptAt, 0L, sweptAt + interval))
    }

    @Test
    fun `a mailbox never swept sweeps once, not once per pass`() {
        // Fresh install, rotated token, and moved host read as 0 and must walk
        // from the beginning. Restore preserves a recent frontier instead.
        assertTrue(relaySweepDue(false, 0L, 0L, 5_000L))
        // ...but a store write that keeps failing must not re-walk forever.
        assertFalse(relaySweepDue(true, 0L, 0L, 5_000L))
    }

    @Test
    fun `a backwards clock sweeps rather than pinning the mailbox`() {
        assertTrue(relaySweepDue(true, 5_000_000L, 0L, 1_000L))
        assertTrue(relaySweepDue(false, 5_000_000L, 0L, 1_000L))
        assertTrue(relaySweepDue(false, Long.MAX_VALUE, 0L, 1_000L))
    }

    @Test
    fun `a completed sweep restarts the interval without costing the frontier`() {
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 9_000L, true)
        assertFalse(store.noteRelaySweepCompleted(key(), 1_000_000L, 9_000L))
        val cursor = store.relayFetchCursor(key())
        assertEquals(9_000L, cursor.afterId)
        assertEquals(1_000_000L, cursor.lastSweepAtMs)
        assertFalse(relaySweepDue(true, cursor.lastSweepAtMs, cursor.sweepAfterId, 1_000_001L))
    }

    @Test
    fun `a sweep starts at zero and a normal pass resumes from the frontier`() {
        assertEquals(0L, relayPassStartCursor(true, 9_000L, 0L))
        assertEquals(9_000L, relayPassStartCursor(false, 9_000L, 0L))
        assertEquals(0L, relayPassStartCursor(false, -5L, 0L))
    }

    // -- the sweep's own resume cursor -----------------------------------

    @Test
    fun `a yielded sweep resumes from its progress instead of restarting at zero`() {
        // The livelock. A walk is bounded (relayMailboxWalkAction), and a
        // sweep is only recorded complete on the empty page that ends the
        // mailbox. On any mailbox holding more than one budget's worth of
        // hint-matching rows the sweep never reached that page, stayed due,
        // and started again at 0 a second later -- the same first 512 rows
        // re-downloaded every few seconds, indefinitely.
        val store = MessageStore.open(":memory:")
        // A long-established mailbox: frontier at the top, last swept six
        // hours ago.
        store.advanceRelayFetchCursor(key(), 29_000L, true)
        store.noteRelaySweepCompleted(key(), 1_000_000L, 29_000L)
        val now = 1_000_000L + relaySweepIntervalMs()

        var cursor = store.relayFetchCursor(key())
        assertTrue(relaySweepDue(true, cursor.lastSweepAtMs, cursor.sweepAfterId, now))
        assertEquals(0L, relayPassStartCursor(true, cursor.afterId, cursor.sweepAfterId))

        // Four pages, then the budget runs out and the pass yields.
        for (pageCursor in listOf(128L, 256L, 384L, 512L)) {
            store.advanceRelaySweepCursor(key(), pageCursor, true, now)
        }

        cursor = store.relayFetchCursor(key())
        assertEquals(512L, cursor.sweepAfterId)
        assertEquals(29_000L, cursor.afterId)
        // Still due -- an unfinished sweep must be finished, whatever the
        // timestamp says -- and it picks up where it stopped.
        assertTrue(relaySweepDue(true, cursor.lastSweepAtMs, cursor.sweepAfterId, now))
        assertEquals(512L, relayPassStartCursor(true, cursor.afterId, cursor.sweepAfterId))
        // An ordinary pass in between still reads the frontier, never this.
        assertEquals(29_000L, relayPassStartCursor(false, cursor.afterId, cursor.sweepAfterId))

        // The empty page ends it: interval restarts, resume cursor cleared.
        store.noteRelaySweepCompleted(key(), now, 29_000L)
        cursor = store.relayFetchCursor(key())
        assertEquals(0L, cursor.sweepAfterId)
        assertEquals(29_000L, cursor.afterId)
        assertFalse(relaySweepDue(true, cursor.lastSweepAtMs, cursor.sweepAfterId, now + 1))
    }

    @Test
    fun `sweep progress obeys the frontier's rule and never slips backwards`() {
        val store = MessageStore.open(":memory:")
        assertEquals(256L, store.advanceRelaySweepCursor(key(), 256L, true, 1_000L))
        // A page that did not reach a terminal disposition for every envelope,
        // or failed to land its acks, must be presented again.
        assertEquals(256L, store.advanceRelaySweepCursor(key(), 512L, false, 1_000L))
        assertEquals(256L, store.relayFetchCursor(key()).sweepAfterId)
        assertEquals(256L, store.advanceRelaySweepCursor(key(), 128L, true, 1_000L))
        // An endpoint with no url or token persists nothing here either.
        assertEquals(0L, store.advanceRelaySweepCursor("", 512L, true, 1_000L))
        assertEquals(0L, store.relayFetchCursor("").sweepAfterId)
    }

    @Test
    fun `a sweep that survives a restart resumes rather than starting over`() {
        // The mesh service is killed and restarted all day. Before the resume
        // cursor, every restart mid-sweep threw the whole walk away.
        val store = MessageStore.open(":memory:")
        store.advanceRelaySweepCursor(key(), 512L, true, 1_000L)
        val cursor = store.relayFetchCursor(key())
        // sweptThisSession is empty again after a restart, but that guard is
        // not what keeps this sweep alive -- the persisted progress is.
        assertTrue(relaySweepDue(false, cursor.lastSweepAtMs, cursor.sweepAfterId, 9_999L))
        assertEquals(512L, relayPassStartCursor(true, cursor.afterId, cursor.sweepAfterId))
    }

    @Test
    fun `a sweep stalled across days offline walks from zero again`() {
        // The rebuilt-relay case. A phone goes offline mid-sweep for days;
        // while it is away the relay is rebuilt from a fresh volume and its
        // row ids restart at 1. The remembered resume cursor now points past
        // the end of the mailbox, so resuming from it would fetch one empty
        // page, record a sweep that covered nothing at all, and put the
        // mailbox back to sleep for another interval while real mail sat below
        // a frontier no ordinary pass goes under.
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 29_000L, true)
        store.noteRelaySweepCompleted(key(), 1_000_000L, 29_000L)
        val sweepStarted = 1_000_000L + relaySweepIntervalMs()
        store.advanceRelaySweepCursor(key(), 20_000L, true, sweepStarted)

        val backOnline = sweepStarted + 3L * 24 * 60 * 60 * 1000
        var cursor = store.relayFetchCursor(key())
        assertTrue(relaySweepDue(false, cursor.lastSweepAtMs, cursor.sweepAfterId, backOnline))
        assertTrue(relaySweepRestartFromZero(cursor.sweepAfterId, cursor.sweepStartedAtMs, backOnline))

        store.resetRelaySweepProgress(key(), backOnline)
        cursor = store.relayFetchCursor(key())
        assertEquals(0L, relayPassStartCursor(true, cursor.afterId, cursor.sweepAfterId))
        // The frontier is not what proved wrong, so it is left alone.
        assertEquals(29_000L, cursor.afterId)

        // ...and the walk that starts here is dated, so the pass a second
        // later resumes it rather than restarting it again. Otherwise the
        // repair would be its own re-download loop.
        store.advanceRelaySweepCursor(key(), 512L, true, backOnline)
        cursor = store.relayFetchCursor(key())
        assertFalse(
            relaySweepRestartFromZero(cursor.sweepAfterId, cursor.sweepStartedAtMs, backOnline + 1_000L),
        )
        assertEquals(512L, relayPassStartCursor(true, cursor.afterId, cursor.sweepAfterId))
    }

    @Test
    fun `a sweep that yielded moments ago resumes rather than restarting`() {
        // The other half, and why this is a staleness question rather than an
        // empty-page one: a walk yields on a fixed budget, so about one sweep
        // in four yields exactly at the end of the mailbox and then resumes
        // into a perfectly honest empty page. Re-walking that from 0 would
        // land on the same boundary again -- the same loop, slower.
        val store = MessageStore.open(":memory:")
        store.advanceRelaySweepCursor(key(), 512L, true, 5_000_000L)
        val cursor = store.relayFetchCursor(key())
        assertEquals(5_000_000L, cursor.sweepStartedAtMs)
        assertFalse(relaySweepRestartFromZero(cursor.sweepAfterId, cursor.sweepStartedAtMs, 5_001_000L))
        assertEquals(512L, relayPassStartCursor(true, cursor.afterId, cursor.sweepAfterId))
    }

    @Test
    fun `abandoning a walk hands the mailbox back to the schedule`() {
        // A relay that answers incoherently -- rows returned, cursor standing
        // still -- ends the walk without completing the sweep. The progress it
        // leaves behind has to go: a mailbox that reads as "a sweep is under
        // way" on every pass never runs an ordinary frontier pass again, so
        // new mail at the top of it would stop arriving altogether.
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 29_000L, true)
        store.advanceRelaySweepCursor(key(), 512L, true, 1_000L)
        var cursor = store.relayFetchCursor(key())
        assertTrue(relaySweepDue(true, cursor.lastSweepAtMs, cursor.sweepAfterId, 2_000L))

        store.resetRelaySweepProgress(key(), 2_000L)
        cursor = store.relayFetchCursor(key())
        assertEquals(0L, cursor.sweepAfterId)
        assertFalse(relaySweepDue(true, cursor.lastSweepAtMs, cursor.sweepAfterId, 2_000L))
        assertEquals(29_000L, relayPassStartCursor(false, cursor.afterId, cursor.sweepAfterId))
    }

    // -- repairing a frontier that outlived its id space ------------------

    @Test
    fun `a completed sweep over a rebuilt mailbox lowers the frontier`() {
        // The operator event, end to end: the relay is rebuilt from a fresh
        // volume and its row ids restart at 1 underneath a frontier of 29000.
        // Ordinary passes ask above the top of the new mailbox and see
        // nothing, and relayd's live push gates on the same value, so the
        // socket is blind too. Only a sweep, which starts at 0, ever reaches
        // that mail -- until the sweep that finds it also fixes the frontier.
        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 29_000L, true)
        store.noteRelaySweepCompleted(key(), 1_000_000L, 29_000L)
        val now = 1_000_000L + relaySweepIntervalMs()
        assertEquals(29_000L, relayPassStartCursor(false, store.relayFetchCursor(key()).afterId, 0L))

        // The sweep walks the new mailbox: two pages ending at id 40, all far
        // below the frontier, so no page can move it.
        for (pageCursor in listOf(16L, 40L)) {
            store.advanceRelaySweepCursor(key(), pageCursor, true, now)
            store.advanceRelayFetchCursor(key(), pageCursor, true)
        }
        assertEquals(29_000L, store.relayFetchCursor(key()).afterId)

        // The empty page ends the walk at after=40, and that is the evidence.
        assertTrue(store.noteRelaySweepCompleted(key(), now, 40L))
        var cursor = store.relayFetchCursor(key())
        assertEquals(40L, cursor.afterId)
        assertEquals(0L, cursor.sweepAfterId)
        // Ordinary delivery is restored, without waiting for another sweep.
        assertEquals(40L, relayPassStartCursor(false, cursor.afterId, cursor.sweepAfterId))
        assertEquals(41L, store.advanceRelayFetchCursor(key(), 41L, true))

        // And it does not repeat: the next completed sweep finds the same top
        // of the same mailbox and writes nothing back.
        assertFalse(store.noteRelaySweepCompleted(key(), now + relaySweepIntervalMs(), 41L))
        cursor = store.relayFetchCursor(key())
        assertEquals(41L, cursor.afterId)
    }

    @Test
    fun `a quiet mailbox and an unfinished sweep both leave the frontier alone`() {
        // The hazard the rule turns on. A drained mailbox and a rebuilt one
        // look almost the same from here, and the empty page carries no
        // evidence either way -- so an empty mailbox never lowers anything.
        // Nothing is lost: mail arriving on a relay that was not rebuilt lands
        // above the frontier where an ordinary pass finds it.
        assertEquals(29_000L, relayFrontierAfterCompletedSweep(29_000L, 0L))
        // A walk that outran a frozen frontier (a page whose envelopes could
        // not all be processed) must not RAISE it -- that would skip the very
        // envelope the freeze exists to re-present.
        assertEquals(5_900L, relayFrontierAfterCompletedSweep(5_900L, 29_000L))

        val store = MessageStore.open(":memory:")
        store.advanceRelayFetchCursor(key(), 29_000L, true)
        assertFalse(store.noteRelaySweepCompleted(key(), 2_000_000L, 0L))
        assertEquals(29_000L, store.relayFetchCursor(key()).afterId)

        // A sweep that yielded on its budget has walked a prefix and knows
        // nothing about the top of the mailbox; it never reaches the repair at
        // all, because only the empty page records a completed sweep.
        for (pageCursor in listOf(128L, 256L, 384L, 512L)) {
            store.advanceRelaySweepCursor(key(), pageCursor, true, 2_000_000L)
        }
        assertEquals(29_000L, store.relayFetchCursor(key()).afterId)
        store.resetRelaySweepProgress(key(), 2_100_000L)
        assertEquals(29_000L, store.relayFetchCursor(key()).afterId)
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
