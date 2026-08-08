import Foundation
import os.log

/// The relay requests one mailbox walk makes, as the walk sees them.
///
/// The whole point of the seam: `MeshController.relaySyncBlocking` supplies
/// these over `RelayClient`, paced and rate-limit-aware through its
/// `relayRequest` wrapper, while a test supplies a scripted page server. The
/// walk itself then needs nothing but a `MessageStore`, so nothing about it is
/// untestable any more.
///
/// Deliberately a struct of closures rather than a protocol: the pass's
/// versions of these are local functions closing over the pass's own state
/// (`ownRelayFault`, the family backoff), which is exactly what a closure
/// captures and exactly what a conforming type would have to be handed anyway.
/// Mirrors `RelayMailboxPages` in RelaySyncEngine.kt.
struct RelayMailboxPages {

    /// One page from `after`. The implementation halves `limit` and retries
    /// the same cursor whenever the answer is too big for this client to take,
    /// and returns the page together with the limit that actually produced it.
    let fetch: (RelayConfig, [Data], Int64, Int) throws -> RelayCappedFetch

    /// Acks these relay row ids.
    let ack: (RelayConfig, [Int64]) throws -> Void

    /// Whether this failure ends the whole multi-mailbox sync pass rather than
    /// only the current page's acks -- the family rate limit, which is a
    /// verdict on the token's budget and so says nothing more can be spent
    /// anywhere.
    let abortsPass: (Error) -> Bool

    /// Records a failure against the config being walked, for the pass's own
    /// relay-health reporting.
    let noteFailure: (RelayConfig, Error) -> Void
}

/// What one mailbox walk leaves for the pass around it to act on. Mirrors
/// `RelayMailboxWalkResult` in RelaySyncEngine.kt.
struct RelayMailboxWalkResult {

    /// Proof this mailbox *answered*, not that the walk was attempted: the
    /// pass uses it as evidence this device's internet works, which is what
    /// licenses resting a contact's silent endpoint.
    let answered: Bool

    /// The walk yielded on its budget having persisted at least one cursor, so
    /// finishing this mailbox is worth a delayed continuation.
    let continuationNeeded: Bool
}

/// The relay mailbox walk, lifted out of `MeshController.relaySyncBlocking` so
/// it can be driven by a test.
///
/// Every walk *decision* already lived in the core (`core/src/relay_cursor.rs`)
/// and was pinned there; what had no test was the wiring that calls those
/// decisions in the right order -- and the sweep livelock fixed in #270 lived
/// precisely in the composition, not in either rule. Deleting the sweep-cursor
/// advance below reinstates it, and no core test notices.
///
/// The counterpart of `RelayMailboxWalker` on Android, case for case. The state
/// that must outlive one pass -- which mailboxes this process has walked in
/// full -- stays in `RelaySweepSession.shared`, where it already was.
final class RelayMailboxWalk {

    private let store: MessageStore
    private let processEnvelope: (RelayFetchedEnvelope, Identity) async -> CoreInboundDisposition

    /// - Parameter processEnvelope: hands one fetched envelope to the inbound
    ///   pipeline and returns what became of it. In the app this hops onto the
    ///   mesh queue, so a relay-fetched envelope is processed under the same
    ///   serial guarantee a BLE frame gets.
    init(
        store: MessageStore,
        processEnvelope: @escaping (RelayFetchedEnvelope, Identity) async -> CoreInboundDisposition
    ) {
        self.store = store
        self.processEnvelope = processEnvelope
    }

    /// Fetches this config's relay mailbox and, per `CoreInboundDisposition`,
    /// either consumes each envelope for good or leaves it be.
    ///
    /// FI2 proxy-polling included: core's `relayFetchHints` is self +
    /// member-group + every contact's recent-day hints, deduped
    /// (core/src/recipient_hints.rs). Proxy-fetched mail this device can't
    /// decrypt falls into the carry-foreign path and comes back `.carried`,
    /// never `.consumed`, so `coreRelayAckIdsWithConsumed` keeps the DTN ack
    /// invariant exactly as before: a carried relay copy is never acked away.
    ///
    /// **Where the walk starts (the persistent frontier).** This used to begin
    /// every pass at `afterId = 0` and page to the end. The un-acked rows above
    /// are left on the relay by design, so a real mailbox only grows, relayd
    /// returns rows in ascending id order, and a *fresh* message therefore has
    /// the highest id and was fetched last -- after every stale row ahead of
    /// it. In the field that reached ~29k rows at 16 rows a page: thousands of
    /// sequential round trips before the newest message was looked at, and
    /// passes that died on a timeout before finishing. Messages took minutes to
    /// arrive.
    ///
    /// A pass now resumes from the frontier persisted for this mailbox and
    /// advances it through `advanceRelayFetchCursor`, which never moves past a
    /// page that failed to fully process or to land its acks, and never moves
    /// backwards -- the mirror of the DTN ack-safety rule applied to skipping.
    /// Occasionally it sweeps the whole mailbox instead, so the rows that are
    /// supposed to stay there remain re-discoverable and a rebuilt relay heals
    /// itself. `relaySweepDue` owns when, from the persisted sweep timestamp:
    /// every `relaySweepIntervalMs`, plus the first pass against a mailbox
    /// never swept at all -- notably NOT every process start, which would tie a
    /// full re-download of the mailbox to the restart rate.
    ///
    /// The walk is bounded, by the same core budget Android uses
    /// (`relayMailboxWalkAction`). This loop used to be a plain `while true`
    /// that ran until the mailbox emptied, so a deep mailbox could hold a whole
    /// sync pass -- and every mailbox queued behind it -- for as long as it
    /// took. It now yields after four pages or 512 envelopes and asks for a
    /// continuation a second later.
    ///
    /// A sweep therefore has to be resumable, and carries its own persisted
    /// cursor (`sweepAfterId`) advanced under the frontier's rule. A sweep is
    /// only recorded complete on the empty page at the end of the mailbox;
    /// restarting it at 0 on every continuation meant any mailbox with more
    /// than one budget's worth of hint-matching rows re-downloaded its first
    /// pages every second, indefinitely, and never completed. The frontier
    /// cannot stand in for it -- it never moves backwards, so on an established
    /// mailbox it says nothing about where the sweep is. That cursor is trusted
    /// only while the id space it names still exists: a sweep still unfinished
    /// a whole interval after it began walks from 0 instead, which is what
    /// keeps a phone that was offline while its relay was rebuilt healing on
    /// its first pass back.
    ///
    /// Mirrors `RelayMailboxWalker.walk` in RelayMailboxWalker.kt.
    func walk(
        config cfg: RelayConfig,
        identity: Identity,
        fetchHints: [Data],
        nowMs now: Int64,
        pages: RelayMailboxPages
    ) async throws -> RelayMailboxWalkResult {
        let cursorKey = relayCursorKey(relayUrl: cfg.relayUrl, relayToken: cfg.relayToken)
        let cursor = try store.relayFetchCursor(configKey: cursorKey)
        let sweeping = relaySweepDue(
            sweptThisSession: RelaySweepSession.shared.hasSwept(cursorKey),
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: now
        )
        // A resume cursor is a row id, and a row id only means anything in the
        // id space it was recorded in. A relay rebuilt from a fresh volume
        // restarts its ids at 1, so a cursor remembered from before it points
        // past the end of the mailbox: the resumed walk would fetch one empty
        // page, read it as end-of-mailbox and record a sweep that covered
        // nothing -- putting the mailbox back to sleep for another interval
        // with real mail sitting below a frontier no ordinary pass goes under.
        // `relaySweepRestartFromZero` decides from the sweep's age rather than
        // from the empty page: a sweep that yielded a second ago is simply
        // finished, while one still unfinished a whole interval after it began
        // has lived through the window in which a relay can be replaced.
        // Mirrors RelaySyncEngine.kt.
        var sweepProgress = cursor.sweepAfterId
        if sweeping, relaySweepRestartFromZero(
            sweepProgressAfterId: sweepProgress,
            sweepStartedAtMs: cursor.sweepStartedAtMs,
            nowMs: now
        ) {
            // Zero the local progress only if the store took the write. If it
            // did not, resuming is the safe answer: a restart that cannot be
            // recorded would happen again next pass, and the pass after that.
            do {
                try store.resetRelaySweepProgress(configKey: cursorKey, nowMs: now)
                relaySyncLog.info(
                    "Relay sweep stalled at after=\(sweepProgress, privacy: .public); restarting the walk from 0"
                )
                sweepProgress = 0
            } catch {
                relaySyncLog.warning(
                    "Failed to restart a stalled relay sweep: \(error.localizedDescription, privacy: .public)"
                )
            }
        }
        var afterId = relayPassStartCursor(
            sweeping: sweeping,
            persistedAfterId: cursor.afterId,
            sweepProgressAfterId: sweepProgress
        )
        // Once a page fails to fully process, both cursors stop moving for the
        // rest of this pass: persisting a later page's cursor would skip the
        // failed one forever. The walk itself continues, so one bad page never
        // blocks the mail behind it.
        var cursorsAdvancing = true
        // Whether this walk wrote any cursor down at all. It is what makes a
        // continuation worth scheduling: a pass that persisted nothing would
        // fetch and fail on exactly the same page a second later.
        var persistedThisWalk = false
        // Not a `let`: a page this client cannot take -- too big to decode, or
        // too big to finish moving over this link -- halves the ask and retries
        // the same cursor, and the reduced limit is kept for the rest of this
        // mailbox's walk rather than reset per page: a mailbox that produced
        // one oversize window usually produces the next one too, and
        // rediscovering that costs a wasted request every page. Scoped to THIS
        // mailbox, exactly as in RelayMailboxWalker.kt -- one relay's oversize
        // page says nothing about the next relay's, and carrying the reduction
        // across configs would shrink every other mailbox's pages for the rest
        // of the pass. The next pass starts from the full limit again.
        var fetchBatchLimit = Int(relayFetchBatchLimit())
        // `answered` in the result means "this mailbox answered", not "the
        // walk was attempted" -- the pass uses it as proof this device's
        // internet works. Every exit below is reached only after a page has
        // come back, so all of them report true; the flag this used to be
        // existed because the loop ended in a `break` and the reporting
        // happened after it.
        var pagesFetched: UInt32 = 0
        var envelopesFetched: UInt32 = 0
        while true {
            let fetched = try pages.fetch(cfg, fetchHints, afterId, fetchBatchLimit)
            let page = fetched.page
            // Carried to the next page of THIS mailbox only; see the
            // declaration above for why.
            fetchBatchLimit = fetched.limit
            guard !page.envelopes.isEmpty else {
                // Records that the walk reached the end of the mailbox:
                // restarts the sweep interval and clears the sweep's resume
                // cursor. Only here -- a sweep cut short by a relay error, a
                // lost network, or simply running out of its per-pass budget
                // leaves both alone, so the next pass finishes it from where
                // this one stopped.
                if sweeping {
                    RelaySweepSession.shared.noteSwept(cursorKey)
                    try? store.noteRelaySweepCompleted(configKey: cursorKey, nowMs: now)
                }
                return RelayMailboxWalkResult(answered: true, continuationNeeded: false)
            }
            pagesFetched += 1
            envelopesFetched += UInt32(clamping: page.envelopes.count)
            var pageFullyProcessed = true
            var dispositions: [CoreRelayEnvelopeDisposition] = []
            for env in page.envelopes {
                let disposition = await processEnvelope(env, identity)
                dispositions.append(CoreRelayEnvelopeDisposition(
                    relayId: env.id,
                    msgId: env.msgId,
                    disposition: disposition,
                    recipientHint: env.recipientHint
                ))
                // A contact-hinted envelope coming out of THIS mailbox is proof
                // the mailbox its recipient polls already holds it (proxy-poll
                // parity: a contact's hints are only ever fetched against that
                // contact's resolved relay). If we also carry the same msg_id
                // from a BLE/LAN encounter, stamp that row so the upload loop
                // stops re-posting a copy the relay demonstrably has (no-op
                // when we carry nothing). Group-hinted rows are deliberately
                // NOT stamped here -- they are stamped only by a complete
                // fan-out post. Bookkeeping only, so a failure must not fail
                // the walk. Mirrors RelayMailboxWalker.kt.
                if let _ = (try? store.contactMatchingHint(hint: env.recipientHint, nowMs: now)) ?? nil {
                    _ = try? store.markCarriedEnvelopeRelayUploaded(
                        msgId: env.msgId,
                        relayUrl: cfg.relayUrl
                    )
                }
            }
            // Consumed/Expired ack unconditionally; a SEEN envelope is acked
            // only if this device durably consumed it as a 1:1 message from
            // someone else (DTN_TODOS.md §3.1); a legacy shared-mailbox
            // group-hint row is never acked at all
            // (specs/group-relay-durability.md §5.2) -- see
            // CoreRelayEnvelopeDisposition's doc comment in engine.rs.
            do {
                let acks = try store.coreRelayAckIdsWithConsumed(
                    items: dispositions,
                    ownUserId: identity.userId,
                    nowMs: now
                )
                // An ack that never landed leaves consumed rows in the mailbox;
                // skipping past them would strand them there until expiry, so
                // the frontier waits for the next pass to retry.
                if !acks.isEmpty {
                    try pages.ack(cfg, acks)
                }
            } catch {
                if pages.abortsPass(error) { throw error }
                pageFullyProcessed = false
                pages.noteFailure(cfg, error)
                relaySyncLog.warning(
                    "Relay page ack failed: \(error.localizedDescription, privacy: .public)"
                )
            }
            if !pageFullyProcessed { cursorsAdvancing = false }
            if cursorsAdvancing {
                persistedThisWalk = true
                _ = try? store.advanceRelayFetchCursor(
                    configKey: cursorKey,
                    pageNextCursor: page.nextCursor,
                    pageFullyProcessed: true
                )
                // Only while sweeping. An ordinary pass writing its page
                // cursors here would leave behind sweep progress claiming
                // coverage of rows no sweep looked at -- and a non-zero
                // progress is also what tells the next pass a sweep is under
                // way.
                if sweeping {
                    _ = try? store.advanceRelaySweepCursor(
                        configKey: cursorKey,
                        pageNextCursor: page.nextCursor,
                        pageFullyProcessed: true,
                        nowMs: now
                    )
                }
            }
            // End the walk on an EMPTY page, never on a short one: a server may
            // clamp `limit=` below our ask, and reading a short page as
            // end-of-mailbox would strand every row above it -- in an
            // ascending-id mailbox, all the new mail. Reaching here with a
            // non-empty page means the cursor stood still, which relayd cannot
            // produce -- a bail-out, not end-of-mailbox, so it deliberately
            // does not record a completed sweep.
            guard relayFetchWalkContinues(
                pageEnvelopeCount: UInt32(clamping: page.envelopes.count),
                afterId: afterId,
                pageNextCursor: page.nextCursor
            ) else {
                relaySyncLog.warning(
                    "Relay returned rows without advancing the cursor; ending the walk"
                )
                // The sweep this walk belonged to is abandoned, not paused, so
                // its progress goes too. Left behind, a non-zero progress reads
                // as "a sweep is under way" on every later pass
                // (`relaySweepDue`), and a mailbox permanently in sweep mode
                // never runs an ordinary frontier pass again -- new mail at the
                // top of it would stop arriving. Clearing it hands the mailbox
                // back to the schedule.
                if sweeping {
                    _ = try? store.resetRelaySweepProgress(configKey: cursorKey, nowMs: now)
                }
                return RelayMailboxWalkResult(answered: true, continuationNeeded: false)
            }
            afterId = page.nextCursor
            // Out of budget: hand the pass back and finish this mailbox from
            // `afterId` a second later. Everything counted here has already
            // reached a terminal disposition and had its cursors written down,
            // so yielding strands nothing.
            if relayMailboxWalkAction(
                pagesFetched: pagesFetched,
                envelopesFetched: envelopesFetched
            ) == .yieldAndScheduleContinuation {
                let continuationNote = persistedThisWalk
                    ? "scheduled"
                    : "declined (nothing persisted)"
                relaySyncLog.info(
                    "Relay mailbox walk yielding after \(pagesFetched, privacy: .public) page(s)/\(envelopesFetched, privacy: .public) envelope(s) at after=\(afterId, privacy: .public); continuation \(continuationNote, privacy: .public)"
                )
                // Only ask for one if this walk actually wrote a cursor down. A
                // pass whose pages could not be processed or acked persists
                // nothing, so the pass a second later starts from the same
                // cursor, fetches the same pages and fails the same way: a
                // 1s-cadence re-download of the same 512 envelopes. The
                // ordinary poll interval retries it instead, and a walk that
                // persisted even one page still gets its continuation. Mirrors
                // RelayMailboxWalker.kt.
                //
                // The delay is armed only once the whole multi-mailbox pass has
                // finished, so this is reported rather than scheduled here:
                // arming it from inside the loop could let the timer fire while
                // a later config is still running and collapse the continuation
                // into an in-flight rerun.
                return RelayMailboxWalkResult(
                    answered: true,
                    continuationNeeded: persistedThisWalk
                )
            }
        }
    }
}
