import XCTest
@testable import CruiseMesh

/// The relay mailbox walk's *wiring*, driven against a real core store and a
/// scripted relay.
///
/// The gap this closes: every walk decision -- where a pass starts, when the
/// cursors may move, when the budget yields, when a sweep is complete -- is a
/// pure core function with its own test, and `RelayFetchCursorTests` calls
/// those functions in the order the shell calls them. That is not the same
/// thing as testing the shell. The livelock fixed in #270 was a composition
/// bug: the per-pass budget was right, the sweep-completion rule was right, and
/// putting them together produced a mailbox that re-downloaded its first pages
/// every second forever. Deleting the one line that advances the sweep cursor
/// reinstates it exactly, and until this file existed every test stayed green.
///
/// Android twin: `RelayMailboxWalkWiringTest.kt`, case for case.
///
/// A pass here is one `RelayMailboxWalk.walk` call. The app runs the next one
/// `relayMailboxContinuationDelayMs` later when the walk asks for it, so that
/// is the clock these advance by.
///
/// Two Android cases have no counterpart here, both because the shells differ
/// rather than because the coverage was skipped. `processInboundEnvelope` on
/// this side cannot throw, so the only way a page fails to reach a terminal
/// disposition is an ack that did not land, and that is the shape used below.
/// And this walk has no per-iteration "is the service still running" guard --
/// Android's loop can stop mid-mailbox when the network goes away -- so there
/// is no stopped-mid-walk case to pin.
final class RelayMailboxWalkWiringTests: XCTestCase {

    private let config = RelayConfig(relayUrl: "https://relay.example", relayToken: "family-token")

    private func cursorKey() -> String {
        relayCursorKey(relayUrl: config.relayUrl, relayToken: config.relayToken)
    }

    override func setUp() {
        super.setUp()
        // The swept-this-process set is a singleton; a leftover entry from
        // another test would suppress the never-swept branch here.
        RelaySweepSession.shared.reset()
    }

    override func tearDown() {
        RelaySweepSession.shared.reset()
        super.tearDown()
    }

    // MARK: - the #270 livelock, as a wiring test

    func testASweepOfADeepMailboxResumesAcrossYieldsAndCompletesExactlyOnce() async throws {
        // The field shape: a long-established mailbox (frontier at the top,
        // swept six hours ago) holding far more hint-matching rows than one
        // pass may take. Everything comes back `.carried` -- the proxy copies
        // and legacy group rows a sweep exists to keep re-discoverable -- so
        // nothing is acked and nothing leaves the mailbox as the walk goes.
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        _ = try harness.store.advanceRelayFetchCursor(
            configKey: cursorKey(),
            pageNextCursor: 29_000,
            pageFullyProcessed: true
        )
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        var now: Int64 = 1_000_000 + relaySweepIntervalMs()
        XCTAssertTrue(try harness.sweepDue(now), "the sweep must be due for this test to test anything")

        var results: [RelayMailboxWalkResult] = []
        var completions = 0
        var lastSweepAt = try harness.cursor().lastSweepAtMs
        // The mailbox needs three passes (100 rows / 10 a page / 4 pages a
        // pass). The guard is what turns the livelock into a failing assertion
        // rather than a hanging test: a sweep that restarts at 0 on every
        // continuation is due forever.
        var stillDue = try harness.sweepDue(now)
        while stillDue {
            guard results.count < 20 else {
                return XCTFail("the sweep never reached the end of the mailbox -- the walk is looping")
            }
            results.append(try await harness.walk(now: now))
            let cursor = try harness.cursor()
            if cursor.lastSweepAtMs != lastSweepAt {
                completions += 1
                lastSweepAt = cursor.lastSweepAtMs
            }
            stillDue = try harness.sweepDue(now)
            if stillDue { now += relayMailboxContinuationDelayMs() }
        }

        // The sweep finished, and finished once.
        XCTAssertEqual(completions, 1)
        XCTAssertEqual(try harness.cursor().lastSweepAtMs, now)
        // ...and the resume cursor is cleared by the completion, so the next
        // pass is an ordinary frontier pass rather than a fourth sweep.
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)
        XCTAssertFalse(try harness.sweepDue(now))

        // Resume, not restart: the cursor asked for never goes backwards across
        // the whole walk, including across the two yields. This is the
        // assertion that fails the moment the sweep-cursor advance is removed
        // -- every continuation would start at 0 again.
        let asked = harness.mailbox.fetches.map { $0.after }
        XCTAssertEqual(asked, asked.sorted())
        XCTAssertEqual(asked.count, Set(asked).count)

        // Three passes: two that yielded on the four-page budget and asked for
        // a continuation, then one that reached the empty page.
        XCTAssertEqual(results.map { $0.continuationNeeded }, [true, true, false])
        XCTAssertTrue(results.allSatisfy { $0.answered })

        // Every row was delivered to the inbound pipeline exactly once. A
        // restarting sweep re-downloads its first pages on every continuation,
        // which is what the field saw ~97 times in fourteen minutes.
        XCTAssertEqual(harness.processed, (1...100).map(Int64.init))

        // The frontier is untouched by a sweep re-reading rows below it.
        XCTAssertEqual(try harness.cursor().afterId, 29_000)
    }

    func testAnUnfinishedSweepKeepsTheMailboxSweepingWhateverTheTimestampSays() async throws {
        // The other half of the livelock's cause: the empty page is the only
        // thing that writes `last_sweep_at`, so between the first yield and the
        // last page the timestamp still describes the *previous* sweep.
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let now: Int64 = 1_000_000 + relaySweepIntervalMs()

        let result = try await harness.walk(now: now)

        XCTAssertTrue(result.continuationNeeded)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 40)
        XCTAssertEqual(try harness.cursor().lastSweepAtMs, 1_000_000)
        XCTAssertTrue(try harness.sweepDue(now + relayMailboxContinuationDelayMs()))
    }

    // MARK: - an ordinary frontier pass

    func testAFrontierPassResumesFromTheFrontierAndWritesNoSweepProgress() async throws {
        // The inverse of the livelock: an ordinary pass that wrote its page
        // cursors into the sweep's resume cursor would leave progress claiming
        // coverage of rows no sweep ever looked at -- and a non-zero progress
        // is also what tells the next pass a sweep is under way, so the mailbox
        // would never leave sweep mode again.
        let harness = try makeHarness(rows: 80, serverPageSize: 10)
        _ = try harness.store.advanceRelayFetchCursor(
            configKey: cursorKey(),
            pageNextCursor: 50,
            pageFullyProcessed: true
        )
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let now: Int64 = 1_000_000 + 1_000
        XCTAssertFalse(try harness.sweepDue(now))

        let result = try await harness.walk(now: now)

        XCTAssertTrue(result.answered)
        XCTAssertFalse(result.continuationNeeded)
        XCTAssertEqual(harness.mailbox.fetches.map { $0.after }, [50, 60, 70, 80])
        XCTAssertEqual(harness.processed, (51...80).map(Int64.init))
        XCTAssertEqual(try harness.cursor().afterId, 80)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)
        XCTAssertEqual(try harness.cursor().lastSweepAtMs, 1_000_000)
    }

    func testADeepFrontierPassYieldsAndItsContinuationPicksUpWhereItStopped() async throws {
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        var now: Int64 = 1_000_000 + 1_000
        XCTAssertFalse(try harness.sweepDue(now))

        let first = try await harness.walk(now: now)
        XCTAssertTrue(first.continuationNeeded)
        XCTAssertEqual(try harness.cursor().afterId, 40)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)

        now += relayMailboxContinuationDelayMs()
        _ = try await harness.walk(now: now)
        XCTAssertEqual(try harness.cursor().afterId, 80)
        XCTAssertEqual(harness.mailbox.fetches.map { $0.after }, [0, 10, 20, 30, 40, 50, 60, 70])
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)
    }

    // MARK: - a page that will not process

    func testAPageThatFailsToAckFreezesBothCursorsWithoutBlockingTheMailBehindIt() async throws {
        // iOS's inbound pipeline cannot throw, so the shape a failed page takes
        // here is the ack that did not land -- the other half of
        // `pageFullyProcessed`, and the one the DTN rule cares about most:
        // consumed rows the relay was never told about must be re-presented,
        // because skipping past them strands them until expiry.
        let harness = try makeHarness(rows: 30, serverPageSize: 10, disposition: .consumed)
        harness.mailbox.failAckOnPage(2)
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let now: Int64 = 1_000_000 + 1_000

        let result = try await harness.walk(now: now)

        XCTAssertTrue(result.answered)
        // The first page landed; nothing after the failed one may be persisted
        // past, which is the DTN ack-safety rule mirrored onto skipping.
        XCTAssertEqual(try harness.cursor().afterId, 10)
        // ...but the walk carried on, so one bad page never strands the mail
        // above it.
        XCTAssertEqual(harness.processed, (1...30).map(Int64.init))
        XCTAssertEqual(harness.mailbox.fetches.map { $0.after }, [0, 10, 20, 30])
        // The first page's rows were acked and left the mailbox; the failed
        // page's rows are still there for the next pass.
        XCTAssertEqual(harness.mailbox.acked, (1...10).map(Int64.init))
        XCTAssertEqual(harness.mailbox.remainingIds(), (11...30).map(Int64.init))
    }

    func testASweepPageThatFailsToAckFreezesTheSweepsResumeCursorToo() async throws {
        // Deep enough that the pass yields before the empty page, so the frozen
        // resume cursor is observable rather than cleared by a completion.
        let harness = try makeHarness(rows: 100, serverPageSize: 10, disposition: .consumed)
        harness.mailbox.failAckOnPage(2)
        let now: Int64 = 5_000_000
        XCTAssertTrue(try harness.sweepDue(now), "a mailbox never swept sweeps on its first pass")

        let result = try await harness.walk(now: now)

        XCTAssertTrue(result.continuationNeeded)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 10)
        XCTAssertEqual(try harness.cursor().afterId, 10)
        // Nothing was recorded as swept: the walk never reached the end.
        XCTAssertEqual(try harness.cursor().lastSweepAtMs, 0)
        // The continuation resumes at the frozen cursor, so the page that
        // failed is presented again rather than skipped.
        let resumed = harness.mailbox.fetches.count
        _ = try await harness.walk(now: now + relayMailboxContinuationDelayMs())
        XCTAssertEqual(harness.mailbox.fetches[resumed].after, 10)
    }

    func testAWalkThatPersistsNothingDeclinesItsContinuation() async throws {
        // #222's shape: a pass that wrote no cursor down would fetch the same
        // pages a second later and fail identically -- a 1s-cadence re-download
        // burning the family rate-limit bucket. The ordinary poll interval
        // retries it instead.
        let harness = try makeHarness(rows: 100, serverPageSize: 10, disposition: .consumed)
        harness.mailbox.failAckOnPage(1)
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let now: Int64 = 1_000_000 + 1_000

        let result = try await harness.walk(now: now)

        XCTAssertTrue(result.answered)
        XCTAssertFalse(result.continuationNeeded)
        XCTAssertEqual(try harness.cursor().afterId, 0)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)

        // A walk that persisted even one page still gets its continuation, so a
        // single bad page costs one retry rather than the whole drain.
        let healthy = try makeHarness(rows: 100, serverPageSize: 10, disposition: .consumed)
        healthy.mailbox.failAckOnPage(2)
        try healthy.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let healthyResult = try await healthy.walk(now: now)
        XCTAssertTrue(healthyResult.continuationNeeded)
    }

    // MARK: - a relay whose cursor stands still

    func testANonAdvancingCursorEndsTheWalkAndHandsTheMailboxBackToTheSchedule() async throws {
        // A relay answering incoherently -- rows returned, cursor unchanged --
        // is a bail-out, not end-of-mailbox. Leaving the sweep's progress
        // behind would make every later pass read "a sweep is under way", and a
        // mailbox permanently in sweep mode never runs an ordinary frontier
        // pass again: new mail at the top of it would stop arriving.
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        _ = try harness.store.advanceRelayFetchCursor(
            configKey: cursorKey(),
            pageNextCursor: 500,
            pageFullyProcessed: true
        )
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        harness.mailbox.freezeCursorFromPage(2)
        var now: Int64 = 1_000_000 + relaySweepIntervalMs()
        XCTAssertTrue(try harness.sweepDue(now))

        let result = try await harness.walk(now: now)

        XCTAssertTrue(result.answered)
        XCTAssertFalse(result.continuationNeeded)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)
        // No completion was recorded: the walk never reached the end.
        XCTAssertEqual(try harness.cursor().lastSweepAtMs, 1_000_000)

        // The consequence, which is why the reset matters: the next sweep
        // starts at the beginning of the mailbox rather than resuming from
        // progress covering rows it never re-read.
        harness.mailbox.unfreezeCursor()
        now += relayMailboxContinuationDelayMs()
        let resumed = harness.mailbox.fetches.count
        _ = try await harness.walk(now: now)
        XCTAssertEqual(harness.mailbox.fetches[resumed].after, 0)
    }

    func testAFrontierPassThatBailsOutLeavesTheSweepScheduleAlone() async throws {
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        harness.mailbox.freezeCursorFromPage(2)
        let now: Int64 = 1_000_000 + 1_000
        XCTAssertFalse(try harness.sweepDue(now))

        _ = try await harness.walk(now: now)

        XCTAssertEqual(try harness.cursor().lastSweepAtMs, 1_000_000)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)
        // The one page that did advance is still persisted.
        XCTAssertEqual(try harness.cursor().afterId, 10)
    }

    // MARK: - the hint set changing under a walk in flight

    func testAHintDigestChangeMidSweepDropsTheProgressSoTheNextPassReWalks() async throws {
        // relayd's next_cursor only ever covers the hints we sent, so mail that
        // arrived under a hint this device did not have yet is already *below*
        // the frontier, where no sweep interval reaches it. Gaining a contact
        // therefore has to throw both cursors away -- including a sweep's
        // progress, whose coverage was computed against the narrower hint set.
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        // The sync pass calls this every pass; the first call only records the
        // digest, so seed it before the hint set changes.
        XCTAssertFalse(try harness.store.noteRelayHintSources(ownUserId: harness.identity.userId))
        let now: Int64 = 5_000_000
        XCTAssertTrue(try harness.sweepDue(now))

        let yielded = try await harness.walk(now: now)
        XCTAssertTrue(yielded.continuationNeeded)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 40)
        XCTAssertEqual(try harness.cursor().afterId, 40)
        let hintsBefore = harness.mailbox.fetches[0].hints.count

        let friend = generateIdentity()
        try harness.store.upsertContact(contact: Contact(
            userId: friend.userId,
            name: "Friend",
            signPk: friend.signPk,
            agreePk: friend.agreePk,
            relayUrl: config.relayUrl,
            relayToken: config.relayToken,
            nickname: nil
        ))
        XCTAssertTrue(try harness.store.noteRelayHintSources(ownUserId: harness.identity.userId))

        XCTAssertEqual(try harness.cursor().sweepAfterId, 0)
        XCTAssertEqual(try harness.cursor().afterId, 0)
        XCTAssertEqual(try harness.cursor().sweepStartedAtMs, 0)

        let resumed = harness.mailbox.fetches.count
        let next = now + relayMailboxContinuationDelayMs()
        XCTAssertTrue(try harness.sweepDue(next))
        _ = try await harness.walk(now: next)
        // Back to the beginning, under the wider hint set.
        XCTAssertEqual(harness.mailbox.fetches[resumed].after, 0)
        XCTAssertGreaterThan(harness.mailbox.fetches[resumed].hints.count, hintsBefore)
    }

    // MARK: - a stalled sweep whose relay may have been rebuilt

    func testASweepStillUnfinishedDaysLaterRestartsFromZeroExactlyOnce() async throws {
        // The rebuilt-relay case, through the wiring: a phone offline for days
        // mid-sweep comes back holding a resume cursor from an id space that
        // may no longer exist. It walks from 0 -- and the restart is dated, so
        // the pass a second later resumes rather than restarting again, which
        // would be the same livelock in a longer costume.
        let harness = try makeHarness(rows: 100, serverPageSize: 10)
        _ = try harness.store.advanceRelayFetchCursor(
            configKey: cursorKey(),
            pageNextCursor: 29_000,
            pageFullyProcessed: true
        )
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let sweepStarted: Int64 = 1_000_000 + relaySweepIntervalMs()
        _ = try harness.store.advanceRelaySweepCursor(
            configKey: cursorKey(),
            pageNextCursor: 20_000,
            pageFullyProcessed: true,
            nowMs: sweepStarted
        )

        var now = sweepStarted + 3 * 24 * 60 * 60 * 1_000
        XCTAssertTrue(try harness.sweepDue(now))
        _ = try await harness.walk(now: now)
        XCTAssertEqual(harness.mailbox.fetches[0].after, 0)
        XCTAssertEqual(try harness.cursor().sweepAfterId, 40)
        XCTAssertEqual(try harness.cursor().sweepStartedAtMs, now)

        let resumed = harness.mailbox.fetches.count
        now += relayMailboxContinuationDelayMs()
        _ = try await harness.walk(now: now)
        XCTAssertEqual(harness.mailbox.fetches[resumed].after, 40)
    }

    // MARK: - the mailbox key the whole walk is filed under

    func testARotatedTokenWalksADifferentMailboxFromTheBeginning() async throws {
        let harness = try makeHarness(rows: 30, serverPageSize: 10)
        try harness.store.noteRelaySweepCompleted(configKey: cursorKey(), nowMs: 1_000_000)
        let now: Int64 = 1_000_000 + 1_000
        _ = try await harness.walk(now: now)
        XCTAssertEqual(try harness.cursor().afterId, 30)

        let rotated = RelayConfig(relayUrl: config.relayUrl, relayToken: "rotated-token")
        XCTAssertNotEqual(cursorKey(), relayCursorKey(relayUrl: rotated.relayUrl, relayToken: rotated.relayToken))
        let resumed = harness.mailbox.fetches.count
        _ = try await harness.walk(now: now, config: rotated)
        XCTAssertEqual(harness.mailbox.fetches[resumed].after, 0)
    }

    // MARK: - harness

    private func makeHarness(
        rows: Int,
        serverPageSize: Int,
        disposition: CoreInboundDisposition = .carried
    ) throws -> WalkHarness {
        let identity = generateIdentity()
        let store = try MessageStore.open(path: ":memory:")
        let hint = try store.relayFetchHints(ownUserId: identity.userId, nowMs: 0)[0]
        let envelopes = (1...rows).map { id in
            RelayFetchedEnvelope(
                id: Int64(id),
                msgId: Data((0..<16).map { UInt8(truncatingIfNeeded: id + $0) }),
                hopTtl: 3,
                recipientHint: hint,
                sealed: Data(repeating: UInt8(truncatingIfNeeded: id), count: 32),
                expiryMs: Int64.max
            )
        }
        return WalkHarness(
            identity: identity,
            store: store,
            mailbox: FakeRelayMailbox(rows: envelopes, serverPageSize: serverPageSize),
            disposition: disposition,
            defaultConfig: config
        )
    }

    /// One mailbox, one store, one walk -- and the record of what the walk did.
    private final class WalkHarness {
        let identity: Identity
        let store: MessageStore
        let mailbox: FakeRelayMailbox
        private let defaultConfig: RelayConfig
        private let walker: RelayMailboxWalk
        private let recorder: Recorder

        /// Relay row ids handed to the inbound pipeline, in order.
        var processed: [Int64] { recorder.ids }

        /// Boxes that list so the walk's escaping closure and the harness can
        /// share it without the closure capturing a half-initialised `self`.
        final class Recorder {
            private(set) var ids: [Int64] = []
            func note(_ id: Int64) { ids.append(id) }
        }

        init(
            identity: Identity,
            store: MessageStore,
            mailbox: FakeRelayMailbox,
            disposition: CoreInboundDisposition,
            defaultConfig: RelayConfig
        ) {
            self.identity = identity
            self.store = store
            self.mailbox = mailbox
            self.defaultConfig = defaultConfig
            let record = Recorder()
            self.walker = RelayMailboxWalk(store: store) { envelope, _ in
                record.note(envelope.id)
                return disposition
            }
            self.recorder = record
        }

        func walk(now: Int64, config: RelayConfig? = nil) async throws -> RelayMailboxWalkResult {
            let cfg = config ?? defaultConfig
            let hints = try store.relayFetchHints(ownUserId: identity.userId, nowMs: now)
            return try await walker.walk(
                config: cfg,
                identity: identity,
                fetchHints: hints,
                nowMs: now,
                pages: mailbox.pages()
            )
        }

        func cursor() throws -> RelayFetchCursor {
            try store.relayFetchCursor(configKey: relayCursorKey(
                relayUrl: defaultConfig.relayUrl,
                relayToken: defaultConfig.relayToken
            ))
        }

        /// The schedule question the walk asks at the top of every pass.
        ///
        /// `sweptThisSession` is false because that is what it is here: the
        /// process-wide set only ever suppresses the *never swept* branch, and
        /// every mailbox in these tests is judged from its persisted row
        /// instead -- which is the state the livelock lived in.
        func sweepDue(_ now: Int64) throws -> Bool {
            let row = try cursor()
            return relaySweepDue(
                sweptThisSession: false,
                lastSweepAtMs: row.lastSweepAtMs,
                sweepProgressAfterId: row.sweepAfterId,
                nowMs: now
            )
        }
    }
}

/// A scripted relay mailbox: rows with ascending ids, served from a cursor the
/// way relayd serves them, with the failure shapes a real relay produces.
///
/// Deliberately not a mock. The walk's correctness is entirely about what it
/// asks for next given what came back, so the fake's job is to answer honestly
/// and record every ask -- `fetches` is what the resume-not-restart assertions
/// read. Android twin: `FakeRelayMailbox` in RelayMailboxWalkWiringTest.kt.
final class FakeRelayMailbox {

    /// One `GET /envelopes` as the relay saw it.
    struct Fetch {
        let after: Int64
        let limit: Int
        let hints: [Data]
    }

    private var rows: [RelayFetchedEnvelope]
    /// How many rows this server will actually put in one page, however many
    /// the client asks for. relayd clamps by its own row and byte budgets, and
    /// a short page must never be read as end-of-mailbox.
    private let serverPageSize: Int
    private var freezeFromPage = Int.max
    private var failAckFromPage = Int.max

    private(set) var fetches: [Fetch] = []
    private(set) var acked: [Int64] = []

    init(rows: [RelayFetchedEnvelope], serverPageSize: Int) {
        self.rows = rows.sorted { $0.id < $1.id }
        self.serverPageSize = serverPageSize
    }

    /// From this page number on, answer with rows but never move the cursor.
    func freezeCursorFromPage(_ page: Int) {
        freezeFromPage = page
    }

    func unfreezeCursor() {
        freezeFromPage = Int.max
    }

    /// Fail the ack issued for this page number and every page after it, as a
    /// dropped connection does.
    func failAckOnPage(_ page: Int) {
        failAckFromPage = page
    }

    func remainingIds() -> [Int64] {
        rows.map { $0.id }
    }

    private func page(after: Int64, limit: Int, hints: [Data]) -> RelayFetchPage {
        fetches.append(Fetch(after: after, limit: limit, hints: hints))
        let window = Array(rows.filter { $0.id > after }.prefix(min(limit, serverPageSize)))
        guard let last = window.last else {
            return RelayFetchPage(envelopes: [], nextCursor: after)
        }
        let next = fetches.count >= freezeFromPage ? after : last.id
        return RelayFetchPage(envelopes: window, nextCursor: next)
    }

    private func ackRows(_ ids: [Int64]) throws {
        if fetches.count >= failAckFromPage {
            throw FakeRelayError.ackFailed
        }
        acked.append(contentsOf: ids)
        rows.removeAll { ids.contains($0.id) }
    }

    /// This mailbox as the walk's transport seam.
    func pages() -> RelayMailboxPages {
        RelayMailboxPages(
            fetch: { _, hints, after, limit in
                RelayCappedFetch(page: self.page(after: after, limit: limit, hints: hints), limit: limit)
            },
            ack: { _, ids in
                try self.ackRows(ids)
            },
            abortsPass: { _ in false },
            noteFailure: { _, _ in }
        )
    }
}

enum FakeRelayError: Error {
    case ackFailed
}
