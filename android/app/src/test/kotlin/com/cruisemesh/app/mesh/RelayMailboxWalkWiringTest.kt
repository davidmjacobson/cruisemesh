package com.cruisemesh.app.mesh

import com.cruisemesh.app.relay.RelayCappedFetch
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayFetchPage
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.relayCursorKey
import uniffi.cruisemesh_core.relayFetchBatchLimit
import uniffi.cruisemesh_core.relayFetchShrunkLimit
import uniffi.cruisemesh_core.relayMailboxContinuationDelayMs
import uniffi.cruisemesh_core.relaySweepDue
import uniffi.cruisemesh_core.relaySweepIntervalMs

/**
 * The relay mailbox walk's *wiring*, driven against a real core store and a
 * scripted relay.
 *
 * The gap this closes: every walk decision -- where a pass starts, when the
 * cursors may move, when the budget yields, when a sweep is complete -- is a
 * pure core function with its own test, and `RelayFetchCursorTest` calls those
 * functions in the order the shell calls them. That is not the same thing as
 * testing the shell. The livelock fixed in #270 was a composition bug: the
 * per-pass budget (#259) was right, the sweep-completion rule was right, and
 * putting them together produced a mailbox that re-downloaded its first pages
 * every second forever. Deleting the one line that advances the sweep cursor
 * reinstates it exactly, and until this file existed every test stayed green.
 *
 * So these drive [RelayMailboxWalker] itself: an in-memory [MessageStore]
 * whose cursor rows are inspected between passes, and a [FakeRelayMailbox]
 * that answers pages the way relayd would -- ascending row ids, a server-side
 * page size below what the client asked for, and the failure shapes a real
 * relay produces (a page that will not process, an ack that does not land, a
 * cursor that stands still).
 *
 * A pass here is one [RelayMailboxWalker.walk] call. The service runs the next
 * one [relayMailboxContinuationDelayMs] later when the walk asks for it, so
 * that is the clock these advance by.
 */
class RelayMailboxWalkWiringTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }
    }

    private val config = RelayConfig("https://relay.example", "family-token")
    private val cursorKey = relayCursorKey(config.relayUrl, config.relayToken)

    // -- the #270 livelock, as a wiring test -----------------------------

    @Test
    fun `a sweep of a mailbox deeper than one pass resumes across yields and completes exactly once`() {
        // The field shape: a long-established mailbox (frontier at the top,
        // swept six hours ago) holding far more hint-matching rows than one
        // pass may take. Everything comes back CARRIED -- the proxy copies and
        // legacy group rows a sweep exists to keep re-discoverable -- so
        // nothing is acked and nothing leaves the mailbox as the walk goes.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.advanceRelayFetchCursor(cursorKey, 29_000L, true)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        var now = 1_000_000L + relaySweepIntervalMs()
        assertTrue("the sweep must be due for this test to be testing anything", harness.sweepDue(now))

        val passes = mutableListOf<RelayMailboxWalkResult>()
        var completions = 0
        var lastSweepAt = harness.cursor().lastSweepAtMs
        // The mailbox needs three passes (100 rows / 10 a page / 4 pages a
        // pass). The guard is what turns the livelock into a failing
        // assertion rather than a hanging test: a sweep that restarts at 0 on
        // every continuation is due forever.
        while (harness.sweepDue(now)) {
            assertTrue(
                "the sweep never reached the end of the mailbox -- the walk is looping",
                passes.size < 20,
            )
            passes += harness.walk(now)
            val cursor = harness.cursor()
            if (cursor.lastSweepAtMs != lastSweepAt) {
                completions += 1
                lastSweepAt = cursor.lastSweepAtMs
            }
            if (harness.sweepDue(now)) now += relayMailboxContinuationDelayMs()
        }

        // The sweep finished, and finished once.
        assertEquals(1, completions)
        assertEquals(now, harness.cursor().lastSweepAtMs)
        // ...and the resume cursor is cleared by the completion, so the next
        // pass is an ordinary frontier pass rather than a fifth sweep.
        assertEquals(0L, harness.cursor().sweepAfterId)
        assertFalse(harness.sweepDue(now))

        // Resume, not restart: the cursor asked for never goes backwards
        // across the whole walk, including across the two yields. This is the
        // assertion that fails the moment the sweep-cursor advance is removed
        // -- every continuation would start at 0 again.
        val asked = harness.mailbox.fetches.map { it.after }
        assertEquals(asked.sorted(), asked)
        assertEquals(asked.distinct(), asked)

        // Three passes: two that yielded on the four-page budget and asked for
        // a continuation, then one that reached the empty page.
        assertEquals(
            listOf(true, true, false),
            passes.take(3).map { it.continuationNeeded },
        )
        assertTrue(passes.take(3).all { it.answered })

        // Every row was delivered to the inbound pipeline exactly once. A
        // restarting sweep re-downloads its first pages on every continuation,
        // which is what the field saw ~97 times in fourteen minutes.
        assertEquals((1L..100L).toList(), harness.processed)

        // The frontier is untouched by a sweep re-reading rows below it.
        assertEquals(29_000L, harness.cursor().afterId)
    }

    @Test
    fun `a sweep that is still unfinished keeps the mailbox sweeping whatever the timestamp says`() {
        // The other half of the livelock's cause: the empty page is the only
        // thing that writes `last_sweep_at`, so between the first yield and
        // the last page the timestamp still describes the *previous* sweep.
        // Abandoning the walk there would strand the resume cursor mid-mailbox
        // and leave the coverage it exists to provide quietly incomplete.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val now = 1_000_000L + relaySweepIntervalMs()

        assertTrue(harness.walk(now).continuationNeeded)
        assertEquals(40L, harness.cursor().sweepAfterId)
        // The timestamp is six hours old and unchanged, and would say "not due"
        // on its own the instant a completion were recorded. The progress is
        // what keeps the mailbox sweeping.
        assertEquals(1_000_000L, harness.cursor().lastSweepAtMs)
        assertTrue(harness.sweepDue(now + relayMailboxContinuationDelayMs()))
    }

    @Test
    fun `only a sweep that reached the end of the mailbox is recorded for this process`() {
        // The walker's own set is the schedule's answer to a store that will
        // not take the completion write: the persisted row then never stops
        // saying "never swept", and this record is the only thing that keeps
        // such a device to one full walk per process instead of one per pass.
        // It guards that branch alone, so only a walk that actually reached the
        // empty page at the end of the mailbox may write it.
        val shallow = harness(rows = 30, serverPageSize = 10)
        val now = 5_000_000L
        assertTrue("a mailbox never swept sweeps on its first pass", shallow.sweepDue(now))
        assertFalse(shallow.sweptThisSession())

        shallow.walk(now)

        assertTrue(shallow.sweptThisSession())
        // What that record buys, in the state it exists for: a row still
        // reading never-swept, which the persisted timestamp alone would sweep
        // on every pass forever.
        assertFalse(relaySweepDue(true, 0L, 0L, now))
        assertTrue(relaySweepDue(false, 0L, 0L, now))

        // A sweep that only yielded on its budget has reached no such
        // conclusion: it has walked part of the mailbox, and the persisted
        // progress is what has to carry the rest.
        val deep = harness(rows = 100, serverPageSize = 10)
        assertTrue(deep.walk(now).continuationNeeded)
        assertFalse(deep.sweptThisSession())
        assertTrue(deep.sweepDue(now + relayMailboxContinuationDelayMs()))
    }

    // -- an ordinary frontier pass ---------------------------------------

    @Test
    fun `a frontier pass resumes from the frontier and writes no sweep progress at all`() {
        // The regression guarded here is the inverse of the livelock: an
        // ordinary pass that wrote its page cursors into the sweep's resume
        // cursor would leave progress claiming coverage of rows no sweep ever
        // looked at -- and a non-zero progress is also what tells the next
        // pass a sweep is under way, so the mailbox would never leave sweep
        // mode again.
        val harness = harness(rows = 80, serverPageSize = 10)
        harness.store.advanceRelayFetchCursor(cursorKey, 50L, true)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val now = 1_000_000L + 1_000L
        assertFalse(harness.sweepDue(now))

        val result = harness.walk(now)

        assertTrue(result.answered)
        assertFalse(result.continuationNeeded)
        // Started at the frontier, not at 0: the whole point of the frontier.
        assertEquals(50L, harness.mailbox.fetches.first().after)
        assertEquals(listOf(50L, 60L, 70L, 80L), harness.mailbox.fetches.map { it.after })
        assertEquals((51L..80L).toList(), harness.processed)
        assertEquals(80L, harness.cursor().afterId)
        // No sweep was running, so nothing sweep-shaped was recorded.
        assertEquals(0L, harness.cursor().sweepAfterId)
        assertEquals(1_000_000L, harness.cursor().lastSweepAtMs)
        assertFalse(harness.sweepDue(now))
    }

    @Test
    fun `a deep frontier pass yields on the budget and the continuation picks up where it stopped`() {
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        var now = 1_000_000L + 1_000L
        assertFalse(harness.sweepDue(now))

        assertTrue(harness.walk(now).continuationNeeded)
        assertEquals(40L, harness.cursor().afterId)
        assertEquals(0L, harness.cursor().sweepAfterId)

        now += relayMailboxContinuationDelayMs()
        harness.walk(now)
        assertEquals(80L, harness.cursor().afterId)
        assertEquals(listOf(0L, 10L, 20L, 30L, 40L, 50L, 60L, 70L), harness.mailbox.fetches.map { it.after })
        assertEquals(0L, harness.cursor().sweepAfterId)
    }

    // -- a page too big for this client to take ---------------------------

    @Test
    fun `an oversize page shrinks the ask for the rest of that mailbox and no further`() {
        // A window whose body blows the response cap is answered with the same
        // cursor at half the limit. The reduction is then kept for the rest of
        // THIS mailbox -- a mailbox that produced one oversize window usually
        // produces the next one too, and rediscovering that costs a wasted
        // round trip on every page -- and no longer, because one relay's
        // oversize page says nothing about the next relay's, and shrinking
        // every other mailbox's pages for the rest of the pass is not a fix
        // for anything.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val full = relayFetchBatchLimit().toInt()
        harness.mailbox.refuseLimitsAbove(full / 4)
        val now = 1_000_000L + 1_000L

        harness.walk(now)

        // Paid once, on the first page, and worked down through the core's own
        // halving rule; every later page of this mailbox asks the reduced
        // limit straight out.
        assertEquals(listOf(full to full / 2, full / 2 to full / 4), harness.mailbox.shrinks)
        assertEquals(List(4) { full / 4 }, harness.mailbox.fetches.map { it.limit })

        // The next mailbox of the same pass pays its own discovery, from the
        // full limit: this walk's reduction was a local of this walk.
        harness.walk(now, config = RelayConfig("https://other.example", "other-token"))
        assertEquals(
            listOf(full to full / 2, full / 2 to full / 4, full to full / 2, full / 2 to full / 4),
            harness.mailbox.shrinks,
        )
    }

    // -- a page that will not process ------------------------------------

    @Test
    fun `a page that fails to process freezes both cursors without blocking the mail behind it`() {
        val harness = harness(rows = 30, serverPageSize = 10)
        harness.failProcessingRow(15L)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val now = 1_000_000L + 1_000L

        val result = harness.walk(now)

        assertTrue(result.answered)
        // The first page landed; nothing after the broken one may be persisted
        // past, which is the DTN ack-safety rule mirrored onto skipping.
        assertEquals(10L, harness.cursor().afterId)
        // ...but the walk carried on, so one bad envelope never strands the
        // mail above it. Everything except the broken row reached the pipeline.
        assertEquals((1L..30L).filter { it != 15L }, harness.processed)
        assertEquals(listOf(0L, 10L, 20L, 30L), harness.mailbox.fetches.map { it.after })
    }

    @Test
    fun `a sweep page that fails to process freezes the sweep's resume cursor too`() {
        // Deep enough that the pass yields before the empty page, so the
        // frozen resume cursor is observable rather than cleared by a
        // completion.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.failProcessingRow(15L)
        val now = 5_000_000L
        assertTrue("a mailbox never swept sweeps on its first pass", harness.sweepDue(now))

        val result = harness.walk(now)

        assertTrue(result.continuationNeeded)
        assertEquals(10L, harness.cursor().sweepAfterId)
        assertEquals(10L, harness.cursor().afterId)
        // Nothing was recorded as swept: the walk never reached the end.
        assertEquals(0L, harness.cursor().lastSweepAtMs)
        // The continuation resumes at the frozen cursor, so the page that
        // failed is presented again rather than skipped.
        val resumed = harness.mailbox.fetches.size
        harness.walk(now + relayMailboxContinuationDelayMs())
        assertEquals(10L, harness.mailbox.fetches[resumed].after)
    }

    @Test
    fun `an ack that does not land freezes the cursors exactly as a failed page does`() {
        // Consumed rows the relay was never told about must be re-presented:
        // skipping past them would strand them in the mailbox until expiry.
        val harness = harness(rows = 30, serverPageSize = 10, disposition = CoreInboundDisposition.CONSUMED)
        harness.mailbox.failAckOnPage(2)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val now = 1_000_000L + 1_000L

        harness.walk(now)

        assertEquals(10L, harness.cursor().afterId)
        // The first page's rows were acked and left the mailbox; the failed
        // page's rows are still there for the next pass.
        assertEquals((1L..10L).toList(), harness.mailbox.acked)
        assertEquals((11L..30L).toList(), harness.mailbox.remainingIds())
    }

    @Test
    fun `a walk that persists nothing declines its continuation`() {
        // #222's shape: a pass that wrote no cursor down would fetch the same
        // pages a second later and fail identically -- a 1s-cadence
        // re-download burning the family rate-limit bucket. The ordinary poll
        // interval retries it instead.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.failProcessingRow(1L)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val now = 1_000_000L + 1_000L

        val result = harness.walk(now)

        assertTrue(result.answered)
        assertFalse(result.continuationNeeded)
        assertEquals(0L, harness.cursor().afterId)
        assertEquals(0L, harness.cursor().sweepAfterId)
        // A walk that persisted even one page still gets its continuation, so
        // a single bad page costs one retry rather than the whole drain.
        val healthy = harness(rows = 100, serverPageSize = 10)
        healthy.failProcessingRow(15L)
        healthy.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        assertTrue(healthy.walk(now).continuationNeeded)
    }

    // -- a relay whose cursor stands still --------------------------------

    @Test
    fun `a non-advancing cursor ends the walk and hands the mailbox back to the schedule`() {
        // A relay answering incoherently -- rows returned, cursor unchanged --
        // is a bail-out, not end-of-mailbox. Leaving the sweep's progress
        // behind would make every later pass read "a sweep is under way", and
        // a mailbox permanently in sweep mode never runs an ordinary frontier
        // pass again: new mail at the top of it would stop arriving.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.advanceRelayFetchCursor(cursorKey, 500L, true)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        harness.mailbox.freezeCursorFromPage(2)
        var now = 1_000_000L + relaySweepIntervalMs()
        assertTrue(harness.sweepDue(now))

        val result = harness.walk(now)

        assertTrue(result.answered)
        assertFalse(result.continuationNeeded)
        assertEquals(0L, harness.cursor().sweepAfterId)
        // No completion was recorded: the walk never reached the end.
        assertEquals(1_000_000L, harness.cursor().lastSweepAtMs)

        // The consequence, which is why the reset matters: the next sweep
        // starts at the beginning of the mailbox rather than resuming from
        // progress covering rows it never re-read.
        harness.mailbox.unfreezeCursor()
        now += relayMailboxContinuationDelayMs()
        val resumed = harness.mailbox.fetches.size
        harness.walk(now)
        assertEquals(0L, harness.mailbox.fetches[resumed].after)
    }

    @Test
    fun `a frontier pass that bails out leaves the sweep schedule alone`() {
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        harness.mailbox.freezeCursorFromPage(2)
        val now = 1_000_000L + 1_000L
        assertFalse(harness.sweepDue(now))

        harness.walk(now)

        assertEquals(1_000_000L, harness.cursor().lastSweepAtMs)
        assertEquals(0L, harness.cursor().sweepAfterId)
        // The one page that did advance is still persisted.
        assertEquals(10L, harness.cursor().afterId)
    }

    // -- the hint set changing under a walk in flight ----------------------

    @Test
    fun `a hint-digest change mid-sweep drops the progress so the next pass re-walks from the start`() {
        // relayd's next_cursor only ever covers the hints we sent, so mail
        // that arrived under a hint this device did not have yet is already
        // *below* the frontier, where no sweep interval reaches it. Gaining a
        // contact therefore has to throw both cursors away -- including a
        // sweep's progress, whose coverage was computed against the narrower
        // hint set.
        val harness = harness(rows = 100, serverPageSize = 10)
        // The sync pass calls this every pass; the first call only records the
        // digest, so seed it before the hint set changes.
        assertFalse(harness.store.noteRelayHintSources(harness.identity.userId))
        val now = 5_000_000L
        assertTrue(harness.sweepDue(now))

        assertTrue(harness.walk(now).continuationNeeded)
        assertEquals(40L, harness.cursor().sweepAfterId)
        assertEquals(40L, harness.cursor().afterId)
        val hintsBefore = harness.mailbox.fetches.first().hints.size

        val friend = generateIdentity()
        harness.store.upsertContact(
            Contact(
                userId = friend.userId,
                name = "Friend",
                signPk = friend.signPk,
                agreePk = friend.agreePk,
                relayUrl = config.relayUrl,
                relayToken = config.relayToken,
            ),
        )
        assertTrue(harness.store.noteRelayHintSources(harness.identity.userId))

        assertEquals(0L, harness.cursor().sweepAfterId)
        assertEquals(0L, harness.cursor().afterId)
        assertEquals(0L, harness.cursor().sweepStartedAtMs)

        val resumed = harness.mailbox.fetches.size
        val next = now + relayMailboxContinuationDelayMs()
        assertTrue(harness.sweepDue(next))
        harness.walk(next)
        // Back to the beginning, under the wider hint set.
        assertEquals(0L, harness.mailbox.fetches[resumed].after)
        assertTrue(harness.mailbox.fetches[resumed].hints.size > hintsBefore)
    }

    // -- a stalled sweep whose relay may have been rebuilt ------------------

    @Test
    fun `a sweep still unfinished days later restarts from zero exactly once`() {
        // The rebuilt-relay case, through the wiring: a phone offline for days
        // mid-sweep comes back holding a resume cursor from an id space that
        // may no longer exist. It walks from 0 -- and the restart is dated, so
        // the pass a second later resumes rather than restarting again, which
        // would be the same livelock in a longer costume.
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.advanceRelayFetchCursor(cursorKey, 29_000L, true)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val sweepStarted = 1_000_000L + relaySweepIntervalMs()
        harness.store.advanceRelaySweepCursor(cursorKey, 20_000L, true, sweepStarted)

        var now = sweepStarted + 3L * 24 * 60 * 60 * 1_000
        assertTrue(harness.sweepDue(now))
        harness.walk(now)
        assertEquals(0L, harness.mailbox.fetches.first().after)
        assertEquals(40L, harness.cursor().sweepAfterId)
        assertEquals(now, harness.cursor().sweepStartedAtMs)

        val resumed = harness.mailbox.fetches.size
        now += relayMailboxContinuationDelayMs()
        harness.walk(now)
        assertEquals(40L, harness.mailbox.fetches[resumed].after)
    }

    // -- the walk stopping because the service did --------------------------

    @Test
    fun `losing the network mid-walk yields nothing to the continuation and strands nothing`() {
        val harness = harness(rows = 100, serverPageSize = 10)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        harness.stopAfterPages(2)
        val now = 1_000_000L + 1_000L

        val result = harness.walk(now)

        assertTrue("two pages answered before the network went", result.answered)
        // No continuation is requested: the pass that stopped for lack of a
        // network cannot ask for one a second later, and the ordinary poll
        // resumes from the cursors these pages did persist.
        assertFalse(result.continuationNeeded)
        assertEquals(20L, harness.cursor().afterId)
    }

    // -- the mailbox key the whole walk is filed under ---------------------

    @Test
    fun `a rotated token walks a different mailbox from the beginning`() {
        val harness = harness(rows = 30, serverPageSize = 10)
        harness.store.noteRelaySweepCompleted(cursorKey, 1_000_000L)
        val now = 1_000_000L + 1_000L
        harness.walk(now)
        assertEquals(30L, harness.cursor().afterId)

        val rotated = RelayConfig(config.relayUrl, "rotated-token")
        assertNotEquals(cursorKey, relayCursorKey(rotated.relayUrl, rotated.relayToken))
        val resumed = harness.mailbox.fetches.size
        harness.walk(now, config = rotated)
        assertEquals(0L, harness.mailbox.fetches[resumed].after)
    }

    // -- harness ----------------------------------------------------------

    private fun harness(
        rows: Int,
        serverPageSize: Int,
        disposition: CoreInboundDisposition = CoreInboundDisposition.CARRIED,
    ): WalkHarness {
        val identity = generateIdentity()
        val store = MessageStore.open(":memory:")
        val hint = store.relayFetchHints(identity.userId, 0L).first()
        val mailbox = FakeRelayMailbox(
            rows = (1..rows).map { id ->
                RelayFetchedEnvelope(
                    id = id.toLong(),
                    msgId = ByteArray(16) { i -> (id + i).toByte() },
                    hopTtl = 3u,
                    recipientHint = hint,
                    sealed = ByteArray(32) { id.toByte() },
                    expiryMs = Long.MAX_VALUE,
                )
            },
            serverPageSize = serverPageSize,
        )
        return WalkHarness(identity, store, mailbox, disposition)
    }

    private inner class WalkHarness(
        val identity: Identity,
        val store: MessageStore,
        val mailbox: FakeRelayMailbox,
        private val disposition: CoreInboundDisposition,
    ) {
        /** Relay row ids handed to the inbound pipeline, in order. */
        val processed = mutableListOf<Long>()
        private var failRow: Long? = null
        private var stopAfterFetches = Int.MAX_VALUE

        private val walker = RelayMailboxWalker(
            store = store,
            processRelayEnvelope = { envelope, _ ->
                if (envelope.id == failRow) error("scripted processing failure for row ${envelope.id}")
                processed += envelope.id
                disposition
            },
            canWalk = { mailbox.fetches.size < stopAfterFetches },
        )

        /** The one envelope whose processing throws, as a corrupt row would. */
        fun failProcessingRow(id: Long) {
            failRow = id
        }

        /** Stops the walk after this many fetches, as a lost network does. */
        fun stopAfterPages(pages: Int) {
            stopAfterFetches = pages
        }

        fun walk(now: Long, config: RelayConfig = this@RelayMailboxWalkWiringTest.config) =
            walker.walk(config, identity, now, mailbox.pages())

        fun cursor() = store.relayFetchCursor(cursorKey)

        /** Whether this process has walked the mailbox to its end, as the walk sees it. */
        fun sweptThisSession() = walker.hasSweptThisSession(cursorKey)

        /**
         * The schedule question the walk asks at the top of every pass, asked
         * the way the walk asks it: from the persisted row *and* the walker's
         * own record of what this process has already walked in full.
         */
        fun sweepDue(now: Long): Boolean {
            val cursor = cursor()
            return relaySweepDue(sweptThisSession(), cursor.lastSweepAtMs, cursor.sweepAfterId, now)
        }
    }
}

/**
 * A scripted relay mailbox: rows with ascending ids, served from a cursor the
 * way relayd serves them, with the failure shapes a real relay produces.
 *
 * Deliberately not a mock. The walk's correctness is entirely about what it
 * asks for next given what came back, so the fake's job is to answer honestly
 * and record every ask -- [fetches] is what the resume-not-restart assertions
 * read.
 */
internal class FakeRelayMailbox(
    rows: List<RelayFetchedEnvelope>,
    /**
     * How many rows this server will actually put in one page, however many
     * the client asks for. relayd clamps by its own row and byte budgets, and
     * a short page must never be read as end-of-mailbox.
     */
    private val serverPageSize: Int = Int.MAX_VALUE,
) {

    /** One `GET /envelopes` as the relay saw it, at the limit that served it. */
    data class Fetch(val after: Long, val limit: Int, val hints: List<ByteArray>)

    private val rows = rows.sortedBy { it.id }.toMutableList()
    val fetches = mutableListOf<Fetch>()
    val acked = mutableListOf<Long>()

    /**
     * Every `(asked, retried with)` pair this mailbox forced, across every
     * walk. One page too big to take costs one halving; paying that again on
     * the next page of the same mailbox is the regression this records.
     */
    val shrinks = mutableListOf<Pair<Int, Int>>()

    private var freezeFromPage = Int.MAX_VALUE
    private var failAckOnPage = Int.MAX_VALUE
    private var maxServableLimit = Int.MAX_VALUE

    /** From this page number on, answer with rows but never move the cursor. */
    fun freezeCursorFromPage(page: Int) {
        freezeFromPage = page
    }

    fun unfreezeCursor() {
        freezeFromPage = Int.MAX_VALUE
    }

    /** Fail the ack issued for this page number, as a dropped connection does. */
    fun failAckOnPage(page: Int) {
        failAckOnPage = page
    }

    /**
     * Refuse any window wider than this, as a page whose body blows the
     * response cap does -- the client's answer is the same cursor at half the
     * limit, and this fake makes it work its way down exactly as
     * [com.cruisemesh.app.relay.RelayClient.fetchEnvelopesWithinResponseCap]
     * does, through the core's own halving rule.
     */
    fun refuseLimitsAbove(limit: Int) {
        maxServableLimit = limit
    }

    fun remainingIds(): List<Long> = rows.map { it.id }

    private fun page(after: Long, limit: Int, hints: List<ByteArray>): RelayFetchPage {
        fetches += Fetch(after, limit, hints)
        val window = rows.filter { it.id > after }.take(minOf(limit, serverPageSize))
        if (window.isEmpty()) return RelayFetchPage(emptyList(), after)
        val next = if (fetches.size >= freezeFromPage) after else window.last().id
        return RelayFetchPage(window, next)
    }

    private fun ackRows(ids: List<Long>) {
        if (fetches.size >= failAckOnPage) throw java.io.IOException("scripted ack failure")
        acked += ids
        rows.removeAll { it.id in ids }
    }

    /** This mailbox as the walk's transport seam. */
    fun pages(): RelayMailboxPages = object : RelayMailboxPages {
        override fun fetch(
            config: RelayConfig,
            hints: List<ByteArray>,
            after: Long,
            limit: Int,
            onShrink: (Int, Int) -> Unit,
        ): RelayCappedFetch {
            var attempt = limit
            while (attempt > maxServableLimit) {
                val smaller = relayFetchShrunkLimit(attempt.toUInt())?.toInt() ?: break
                shrinks += attempt to smaller
                onShrink(attempt, smaller)
                attempt = smaller
            }
            return RelayCappedFetch(page(after, attempt, hints), attempt)
        }

        override fun ack(config: RelayConfig, relayIds: List<Long>) = ackRows(relayIds)

        override fun abortsPass(error: Exception): Boolean = false
    }
}
