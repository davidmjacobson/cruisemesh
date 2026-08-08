package com.cruisemesh.app.mesh

import android.util.Log
import androidx.annotation.VisibleForTesting
import com.cruisemesh.app.relay.RelayCappedFetch
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreRelayEnvelopeDisposition
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.RelayMailboxWalkAction
import uniffi.cruisemesh_core.relayCursorKey
import uniffi.cruisemesh_core.relayFetchBatchLimit
import uniffi.cruisemesh_core.relayFetchWalkContinues
import uniffi.cruisemesh_core.relayMailboxWalkAction
import uniffi.cruisemesh_core.relayPassStartCursor
import uniffi.cruisemesh_core.relaySweepDue
import uniffi.cruisemesh_core.relaySweepRestartFromZero
import java.util.concurrent.ConcurrentHashMap

// Deliberately MeshService's tag rather than this file's name, for the same
// reason RelaySyncEngine keeps it: field tooling (logcat filters, the
// debug-report scripts) matches on "MeshService" for relay-sync lines, and
// these lines were emitted under it before the walk moved into its own class.
private const val TAG = "MeshService"

/**
 * The relay requests one mailbox walk makes, as the walk sees them.
 *
 * The whole point of the seam: [RelaySyncEngine] implements this over
 * [com.cruisemesh.app.relay.RelayClient], pinned to the pass's bound network
 * and paced/backed-off by its `relayRequest` wrapper, while a test implements
 * it with a scripted page server. Nothing else about the walk needs a
 * [android.content.Context], a [android.net.ConnectivityManager], or a live
 * relay, so nothing else about it was untestable.
 */
internal interface RelayMailboxPages {

    /**
     * One page from `after`, halving `limit` and retrying the same cursor
     * whenever the answer is too big for this client to take (`onShrink` is
     * called with the limit that failed and the smaller one being tried).
     * Returns the page together with the limit that actually produced it.
     */
    fun fetch(
        config: RelayConfig,
        hints: List<ByteArray>,
        after: Long,
        limit: Int,
        onShrink: (Int, Int) -> Unit,
    ): RelayCappedFetch

    /** Acks these relay row ids. */
    fun ack(config: RelayConfig, relayIds: List<Long>)

    /**
     * Whether this failure ends the whole multi-mailbox sync pass rather than
     * only the current page's acks -- the family rate limit, which is a verdict
     * on the token's budget and so says nothing more can be spent anywhere.
     */
    fun abortsPass(error: Exception): Boolean
}

/** What one mailbox walk leaves for the pass around it to act on. */
internal data class RelayMailboxWalkResult(
    /**
     * Proof this mailbox *answered*, not that the walk was attempted: the pass
     * uses it as evidence this device's internet works, which is what licenses
     * resting a contact's silent endpoint.
     */
    val answered: Boolean,
    /**
     * The walk yielded on its budget having persisted at least one cursor, so
     * finishing this mailbox is worth a delayed continuation.
     */
    val continuationNeeded: Boolean,
) {
    companion object {
        val NOT_ANSWERED = RelayMailboxWalkResult(answered = false, continuationNeeded = false)
    }
}

/**
 * The relay mailbox walk, lifted out of [RelaySyncEngine] so it can be driven
 * by a test.
 *
 * Every walk *decision* already lived in the core (`core/src/relay_cursor.rs`)
 * and was pinned there; what had no test was the wiring that calls those
 * decisions in the right order -- and the livelock fixed in #270 lived
 * precisely in the composition, not in either rule. Deleting the sweep-cursor
 * advance below reinstates that livelock, and no core test notices.
 *
 * State that must outlive one pass ([sweptThisSession]) lives here; everything
 * per-pass is a parameter or a local, exactly as it was when this was a method.
 */
internal class RelayMailboxWalker(
    private val store: MessageStore,
    private val processRelayEnvelope: (RelayFetchedEnvelope, Identity) -> CoreInboundDisposition,
    /** `isRunning() && hasValidatedInternet()` in the service; the walk stops the moment it goes false. */
    private val canWalk: () -> Boolean,
) {

    /**
     * Mailboxes this process has already walked in full.
     *
     * Deliberately in-memory, and deliberately *narrow*: [relaySweepDue]
     * schedules from the persisted timestamp, and consults this only for a
     * mailbox that has never recorded a completed sweep. There it stops a
     * store write that keeps failing from turning every pass into a full
     * walk. A cold start on a mailbox with a recent sweep no longer re-walks
     * anything.
     *
     * Its meaning is unchanged by the sweep resume cursor, and the two do not
     * overlap. This bounds the cost of a store that cannot be written to --
     * where nothing is persisted, so `sweepAfterId` reads 0 and cannot keep a
     * sweep due. Persisted progress bounds the cost of a mailbox too deep to
     * walk in one pass, which is a store working exactly as intended. One
     * failing write still costs one walk per process; it does not cost one
     * per pass, and it never did once this set was consulted.
     */
    private val sweptThisSession: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /**
     * Fetches this config's relay mailbox and, per [CoreInboundDisposition],
     * either consumes each envelope for good or leaves it be.
     *
     * The fetch itself covers two disjoint concerns, combined into one hint
     * set so they ride the same paginated fetch:
     * [MessageStore.relaySelfHints] (mail addressed to us, pairwise or
     * via a group we belong to) and [MessageStore.relayProxyHints] (mail
     * addressed to a *contact*, fetched on their behalf -- relay
     * proxy-polling, see that function's doc for why this is the fix for "a
     * 1:1 message to a WiFi-less recipient never bridges across BLE
     * clusters"). Every fetched envelope still goes through
     * [processRelayEnvelope] -> [InboundEnvelopeProcessor.processInboundEnvelope]
     * exactly as before; what's new is that the ack decision now follows the
     * returned [CoreInboundDisposition] via
     * [MessageStore.coreRelayAckIdsWithConsumed] instead of unconditionally
     * acking everything the fetch returned. A proxied envelope comes back as
     * CARRIED, not CONSUMED, so it is deliberately left on the relay --
     * [InboundEnvelopeProcessor.carryRelayEnvelope] already queued it for BLE
     * delivery to its real recipient, and the relay copy remains the durable
     * fallback until they (or another proxy) fetch and consume it, or it
     * expires. A SEEN envelope this device already consumed as a 1:1 message
     * over BLE/LAN is also acked (DTN_TODOS.md §3.1) instead of being
     * re-fetched on every pass until expiry -- including receipts and the
     * other service kinds that leave no `messages` row, on the strength of
     * the consumed-set [InboundEnvelopeProcessor] records under the same
     * sole-endpoint-consumer rule. See [CoreRelayEnvelopeDisposition]'s KDoc
     * and [MessageStore.coreRecordConsumedHiddenMsgId] for the exact rule.
     *
     * ### Where the walk starts (the persistent frontier)
     *
     * This used to start every pass at `after = 0` and page forward to the
     * end. The un-acked rows above are left on the relay *by design*, so a
     * real mailbox only grows, relayd returns rows in ascending id order,
     * and a **fresh** message therefore has the highest id and was fetched
     * last -- after every stale row ahead of it. In the field that reached
     * ~29k rows at 16 rows a page: thousands of sequential HTTP round trips
     * before the newest message was looked at, and passes that regularly died
     * on a timeout before finishing. Messages took minutes to arrive.
     *
     * A pass now resumes from the frontier persisted for this mailbox
     * ([MessageStore.relayFetchCursor], keyed by [relayCursorKey]) and
     * advances it per [MessageStore.advanceRelayFetchCursor] -- which never
     * moves past a page that did not reach a terminal disposition for every
     * envelope *and* land its acks, and never moves backwards. That is the
     * mirror of the DTN ack-safety rule applied to skipping: an envelope
     * whose processing threw must be re-presented next pass, so nothing may
     * be persisted past it.
     *
     * A pass is also bounded by [relayMailboxWalkAction], whose budget lives
     * in the core so iOS bounds its walk with the same numbers. This matters
     * when a legacy/current backup restores without `relay_fetch_cursors`:
     * starting at zero is correct, but must not synchronously drain an
     * arbitrarily deep mailbox. Safe pages advance the durable cursors, then
     * the walk yields and schedules a delayed continuation from that point.
     *
     * Occasionally the pass sweeps instead -- walks the whole mailbox rather
     * than only what is new -- so those deliberately-unacked rows stay
     * re-discoverable for the phones that depend on this one re-offering
     * them over Bluetooth, and so a relay rebuilt with its row ids restarted
     * at 1 heals itself. [relaySweepDue] owns when, from the *persisted*
     * sweep timestamp: every [uniffi.cruisemesh_core.relaySweepIntervalMs],
     * plus the first pass against a mailbox never swept at all. Notably NOT
     * every process start -- this service is killed and restarted all day, a
     * sweep re-downloads the sealed body of every row still in the mailbox,
     * and tying that to the restart rate made the interval meaningless.
     *
     * A sweep also carries its own resume cursor
     * ([uniffi.cruisemesh_core.RelayFetchCursor.sweepAfterId], advanced through
     * [MessageStore.advanceRelaySweepCursor] under the frontier's rule) and
     * resumes from it rather than restarting at 0. It has to: the budget above
     * hands a deep mailbox back after four pages, and a sweep is only recorded
     * complete on the empty page at the end of it. Restarting at 0 on every
     * continuation meant any mailbox holding more than one budget's worth of
     * hint-matching rows re-downloaded the same first pages every second or
     * so, indefinitely, and never finished a sweep at all. The frontier cannot
     * stand in for that cursor -- it never moves backwards, so on a
     * long-established mailbox it says nothing about where the sweep is.
     *
     * That cursor is trusted only while the id space it names still exists. A
     * sweep still unfinished a whole interval after it began -- a phone that
     * was offline for days, which is also the window in which a relay gets
     * rebuilt with its row ids restarted at 1 -- walks from 0 instead
     * ([relaySweepRestartFromZero]). Without that, the first pass back would
     * resume past the end of the new mailbox, take the empty page as proof the
     * mailbox had been swept, and go quiet for another interval while real
     * mail sat below the frontier.
     *
     * TODO(relay-proxy-polling follow-up): [MessageStore.relayProxyHints]
     * fetches every contact's hints on every pass, so its cost scales with
     * contact-list size. Fine for this app's small family circles; would need
     * a smarter server-side "for this family token" fan-out if that ever
     * became a large flat social graph.
     */
    fun walk(
        config: RelayConfig,
        identity: Identity,
        now: Long,
        pages: RelayMailboxPages,
    ): RelayMailboxWalkResult {
        val fetchHints = store.relayFetchHints(identity.userId, now)
        if (fetchHints.isEmpty()) return RelayMailboxWalkResult.NOT_ANSWERED
        val cursorKey = relayCursorKey(config.relayUrl, config.relayToken)
        val cursor = store.relayFetchCursor(cursorKey)
        val sweeping = relaySweepDue(
            sweptThisSession.contains(cursorKey),
            cursor.lastSweepAtMs,
            cursor.sweepAfterId,
            now,
        )
        // A resume cursor is a row id, and a row id only means anything in the
        // id space it was recorded in. A relay rebuilt from a fresh volume
        // restarts its ids at 1, so a cursor remembered from before that points
        // past the end of the mailbox: the resumed walk would fetch one empty
        // page, read it as end-of-mailbox, and record a sweep that covered
        // nothing -- putting the mailbox back to sleep for another interval
        // with real mail sitting below a frontier no ordinary pass goes under.
        // [relaySweepRestartFromZero] decides, from the sweep's age rather than
        // from the empty page: a sweep that yielded a second ago is simply
        // finished, while one still unfinished a whole interval after it began
        // has been through the window in which a relay can be replaced.
        var sweepProgress = cursor.sweepAfterId
        if (sweeping && relaySweepRestartFromZero(sweepProgress, cursor.sweepStartedAtMs, now)) {
            // Zero the local progress only if the store took the write. If it
            // did not, resuming is the safe answer: restarting a walk whose
            // restart cannot be recorded would restart it again next pass, and
            // again after that.
            try {
                store.resetRelaySweepProgress(cursorKey, now)
                Log.i(
                    TAG,
                    "Relay ${config.relayUrl} sweep has been stalled at after=$sweepProgress since " +
                        "${cursor.sweepStartedAtMs}; restarting the walk from 0",
                )
                sweepProgress = 0L
            } catch (e: CoreException) {
                Log.w(TAG, "Failed to restart the stalled sweep on ${config.relayUrl}: ${e.message}")
            }
        }
        var after = relayPassStartCursor(sweeping, cursor.afterId, sweepProgress)
        // Once any page fails to fully process, both cursors stop moving for
        // the rest of this pass -- persisting a later page's cursor would
        // skip the failed one forever. The walk itself continues, so one bad
        // envelope never blocks the mail behind it.
        var cursorsAdvancing = true
        // Whether this walk wrote any cursor down at all. It is what makes a
        // continuation worth scheduling: a pass that persisted nothing would
        // fetch and fail on exactly the same page a second later.
        var persistedThisWalk = false
        // Not a val: a page this client cannot take halves the ask and retries
        // the same cursor, and the reduced limit is kept for the rest of this
        // mailbox's walk rather than reset per page -- a mailbox that produced
        // one oversize window usually produces the next one too, and
        // rediscovering that costs a wasted request every page. It is a local
        // of this function and so scoped to THIS mailbox, exactly as in
        // MeshController.swift: one relay's oversize page says nothing about
        // the next relay's, and carrying the reduction across configs would
        // shrink every other mailbox's pages too. The next pass starts from
        // the full limit again.
        var fetchBatchLimit = relayFetchBatchLimit().toInt()
        Log.i(
            TAG,
            "Relay mailbox walk on ${config.relayUrl}: ${if (sweeping) "sweep" else "frontier"} from after=$after",
        )
        // Set the moment a page comes back: the caller uses this as proof that
        // this device's internet works, so it must mean "this mailbox
        // answered", not "the walk was attempted".
        var answered = false
        var pagesFetched = 0u
        var envelopesFetched = 0u
        while (canWalk()) {
            val fetched = pages.fetch(config, fetchHints, after, fetchBatchLimit) { tried, smaller ->
                Log.w(
                    TAG,
                    "Relay ${config.relayUrl} page after=$after was too big to take at limit=$tried; " +
                        "retrying with limit=$smaller",
                )
            }
            val page = fetched.page
            fetchBatchLimit = fetched.limit
            answered = true
            Log.i(
                TAG,
                "Fetched ${page.envelopes.size} relay envelope(s) from ${config.relayUrl} after=$after next=${page.nextCursor}",
            )
            if (page.envelopes.isEmpty()) {
                if (sweeping) noteSweepCompleted(cursorKey, now)
                return RelayMailboxWalkResult(answered = true, continuationNeeded = false)
            }
            pagesFetched += 1u
            envelopesFetched += page.envelopes.size.toUInt()
            var pageFullyProcessed = true
            val dispositions = ArrayList<CoreRelayEnvelopeDisposition>(page.envelopes.size)
            for (envelope in page.envelopes) {
                val disposition = try {
                    processRelayEnvelope(envelope, identity)
                } catch (e: Exception) {
                    // Terminal for this page's cursor purposes only in the
                    // negative sense: we do NOT know what happened to this
                    // envelope, so the frontier must not pass it.
                    pageFullyProcessed = false
                    Log.w(
                        TAG,
                        "Failed to process relay envelope id=${envelope.id} from ${config.relayUrl}: ${e.message}",
                    )
                    continue
                }
                dispositions += CoreRelayEnvelopeDisposition(
                    relayId = envelope.id,
                    msgId = envelope.msgId,
                    disposition = disposition,
                    recipientHint = envelope.recipientHint,
                )
                // A contact-hinted envelope coming out of THIS mailbox is
                // proof the mailbox its recipient polls already holds it
                // (proxy-poll parity: a contact's hints are only ever fetched
                // against that contact's resolved relay). If we also carry
                // the same msg_id from a BLE/LAN encounter, stamp that row so
                // the upload loop stops re-posting a copy the relay
                // demonstrably has (no-op when we carry nothing). Group-hinted
                // rows are deliberately NOT stamped here: one mailbox holding
                // a legacy shared row says nothing about the other members'
                // mailboxes the fan-out still owes -- they are stamped only by
                // a complete fan-out post above. Bookkeeping only: a failure
                // here must not fail the walk.
                try {
                    if (store.contactMatchingHint(envelope.recipientHint, now) != null) {
                        store.markCarriedEnvelopeRelayUploaded(envelope.msgId, config.relayUrl)
                    }
                } catch (e: CoreException) {
                    Log.w(TAG, "Failed to stamp fetched envelope as relay-held: ${e.message}")
                }
            }
            // Consumed/Expired ack unconditionally; a SEEN envelope is
            // acked only if this device durably consumed it as a 1:1
            // message from someone else (DTN_TODOS.md §3.1); a legacy
            // shared-mailbox group-hint row is never acked at all
            // (specs/group-relay-durability.md §5.2) -- see
            // CoreRelayEnvelopeDisposition's KDoc.
            val ackIds = store.coreRelayAckIdsWithConsumed(dispositions, identity.userId, now)
            if (ackIds.isNotEmpty()) {
                Log.i(TAG, "Acking ${ackIds.size} relay envelope(s) on ${config.relayUrl}: $ackIds")
                // An ack that never landed leaves consumed rows in the
                // mailbox; skipping past them would strand them there until
                // expiry, so the frontier waits for the next pass to retry.
                try {
                    pages.ack(config, ackIds)
                } catch (e: Exception) {
                    if (pages.abortsPass(e)) throw e
                    pageFullyProcessed = false
                    Log.w(TAG, "Failed to ack relay envelope(s) on ${config.relayUrl}: ${e.message}")
                }
            }
            if (!pageFullyProcessed) cursorsAdvancing = false
            if (cursorsAdvancing) {
                persistedThisWalk = true
                store.advanceRelayFetchCursor(cursorKey, page.nextCursor, true)
                // Only while sweeping. An ordinary pass writing its page
                // cursors here would leave behind sweep progress claiming
                // coverage of rows no sweep looked at -- and a non-zero
                // progress is also what tells the next pass a sweep is under
                // way.
                if (sweeping) store.advanceRelaySweepCursor(cursorKey, page.nextCursor, true, now)
            }
            // End the walk on an EMPTY page, never on a short one: a server
            // is free to clamp `limit=` below our ask, and reading a short
            // page as end-of-mailbox would strand every row above it -- which
            // in an ascending-id mailbox is all the new mail. Reaching here
            // with a non-empty page means the cursor stood still, which relayd
            // cannot produce -- so this is a bail-out, not end-of-mailbox, and
            // deliberately does NOT record a completed sweep.
            if (!relayFetchWalkContinues(page.envelopes.size.toUInt(), after, page.nextCursor)) {
                Log.w(TAG, "Relay ${config.relayUrl} returned rows without advancing the cursor; ending the walk")
                // The sweep this walk belonged to is abandoned, not paused, so
                // its progress goes too. Left behind, a non-zero progress reads
                // as "a sweep is under way" on every later pass ([relaySweepDue]),
                // and a mailbox permanently in sweep mode never runs an ordinary
                // frontier pass again -- new mail at the top of it would stop
                // arriving. Clearing it hands the mailbox back to the schedule.
                if (sweeping) {
                    try {
                        store.resetRelaySweepProgress(cursorKey, now)
                    } catch (e: CoreException) {
                        Log.w(TAG, "Failed to clear abandoned sweep progress on ${config.relayUrl}: ${e.message}")
                    }
                }
                return RelayMailboxWalkResult(answered = true, continuationNeeded = false)
            }
            after = page.nextCursor
            if (
                relayMailboxWalkAction(pagesFetched, envelopesFetched) ==
                RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION
            ) {
                Log.i(
                    TAG,
                    "Relay ${config.relayUrl} mailbox walk yielding after " +
                        "$pagesFetched page(s)/$envelopesFetched envelope(s) at after=$after; " +
                        "continuation ${if (persistedThisWalk) "scheduled" else "declined (nothing persisted)"}",
                )
                // Only ask for one if this walk actually wrote a cursor down.
                // A pass whose pages could not be processed or acked persists
                // nothing, so the pass a second later starts from the same
                // cursor, fetches the same pages and fails the same way: a
                // 1s-cadence re-download of the same 512 envelopes, burning
                // the family rate-limit bucket in the shape the carry
                // re-upload storm (#222) did. Nothing is lost by declining --
                // the ordinary poll interval retries it, by which time the
                // relay or the envelope that broke may have changed -- and a
                // walk that persisted even one page still gets its
                // continuation, so a single bad page costs one retry rather
                // than the drain.
                //
                // Start the delay only after the entire multi-mailbox pass
                // finishes. Scheduling here could let the timer fire while a
                // later config is still running and collapse the continuation
                // into an immediate in-flight rerun.
                return RelayMailboxWalkResult(answered = true, continuationNeeded = persistedThisWalk)
            }
        }
        return RelayMailboxWalkResult(answered = answered, continuationNeeded = false)
    }

    /**
     * Whether this process has already walked this mailbox to its end -- the
     * `swept_this_session` [relaySweepDue] reads at the top of every pass.
     *
     * Exposed because the schedule question the walk asks cannot be
     * reconstructed from the store: [sweptThisSession] is exactly the part of
     * that question the store does not hold, and what it guards against is a
     * store that will not take the completion write, where the persisted row
     * therefore never stops saying "never swept". Mirrors
     * `RelaySweepSession.hasSwept` on iOS, which is separately addressable for
     * the same reason.
     */
    @VisibleForTesting
    fun hasSweptThisSession(cursorKey: String): Boolean = sweptThisSession.contains(cursorKey)

    /**
     * Records that a walk reached the end of this mailbox: restarts the sweep
     * interval and clears the sweep's resume cursor. Only called on natural
     * termination -- a sweep cut short by the service stopping, the network
     * going away, a relay error, or simply running out of its per-pass budget
     * leaves the timestamp alone, so the next pass finishes the sweep from
     * where this one stopped rather than believing a partial re-walk.
     */
    private fun noteSweepCompleted(cursorKey: String, now: Long) {
        sweptThisSession.add(cursorKey)
        store.noteRelaySweepCompleted(cursorKey, now)
    }
}
