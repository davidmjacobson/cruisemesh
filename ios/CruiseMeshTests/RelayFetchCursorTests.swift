import XCTest
@testable import CruiseMesh

/// The frontier that stopped every relay sync pass from re-walking the whole
/// mailbox from id 0.
///
/// The bug these pin: a relay mailbox legitimately keeps rows nobody will ever
/// ack (a proxy-fetched copy stays as the durable fallback; a legacy group-hint
/// row is never acked at all), relayd returns rows in ascending id order, and a
/// *fresh* message therefore has the highest id and is fetched last. Restarting
/// at 0 every pass meant paging through everything stale before reaching
/// anything new -- minutes of delivery latency on a real mailbox, and passes
/// that timed out before finishing.
///
/// And the sweep's own resume cursor, which is the second half of the same
/// story: the walk is bounded per pass, a sweep is only recorded complete on
/// the empty page at the end of the mailbox, so a sweep that restarted at 0 on
/// every yield could never finish on a mailbox big enough to need the bound.
///
/// Android twin: `RelayFetchCursorTest.kt`, case for case.
final class RelayFetchCursorTests: XCTestCase {
    private let url = "https://relay.example"
    private let token = "member-token"

    private func key() -> String {
        relayCursorKey(relayUrl: url, relayToken: token)
    }

    // MARK: - mailbox identity

    func testAMailboxKeyIsStableAcrossUrlSpellingsAndCarriesNoCredential() {
        XCTAssertEqual(key(), relayCursorKey(relayUrl: "relay.example/", relayToken: "  member-token  "))
        XCTAssertFalse(key().contains(token))
        XCTAssertFalse(key().contains("relay.example"))
    }

    func testRotatingTheTokenNamesADifferentMailboxSoTheCursorStartsOver() throws {
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 9_000, pageFullyProcessed: true)

        let rotated = relayCursorKey(relayUrl: url, relayToken: "rotated-token")
        XCTAssertNotEqual(key(), rotated)
        XCTAssertEqual(try store.relayFetchCursor(configKey: rotated).afterId, 0)
        XCTAssertEqual(try store.relayFetchCursor(configKey: rotated).lastSweepAtMs, 0)
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).afterId, 9_000)
    }

    func testTwoFamiliesOnOneHostDoNotShareACursor() {
        XCTAssertNotEqual(
            relayCursorKey(relayUrl: url, relayToken: "family-one"),
            relayCursorKey(relayUrl: url, relayToken: "family-two")
        )
    }

    // MARK: - advance / do-not-advance policy

    func testAFullyProcessedPageAdvancesThePersistedFrontier() throws {
        let store = try MessageStore.open(path: ":memory:")
        XCTAssertEqual(
            try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 256, pageFullyProcessed: true),
            256
        )
        XCTAssertEqual(
            try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 512, pageFullyProcessed: true),
            512
        )
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).afterId, 512)
    }

    func testAPageThatFailedMidWayNeverMovesTheFrontierPastIt() throws {
        // The mirror of the DTN ack-safety invariant: an envelope whose
        // processing or ack failed must be re-presented next pass, which can
        // only happen if nothing was persisted past it.
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 256, pageFullyProcessed: true)
        XCTAssertEqual(
            try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 512, pageFullyProcessed: false),
            256
        )
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).afterId, 256)
        XCTAssertEqual(
            relayCursorAdvance(persistedAfterId: 256, pageNextCursor: 512, pageFullyProcessed: false),
            256
        )
        XCTAssertEqual(
            relayCursorAdvance(persistedAfterId: 256, pageNextCursor: 512, pageFullyProcessed: true),
            512
        )
    }

    func testASweepReReadingOldPagesNeverRewindsTheFrontier() throws {
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 9_000, pageFullyProcessed: true)
        for pageCursor: Int64 in [256, 512, 8_000] {
            _ = try store.advanceRelayFetchCursor(
                configKey: key(),
                pageNextCursor: pageCursor,
                pageFullyProcessed: true
            )
        }
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).afterId, 9_000)
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 9_500, pageFullyProcessed: true)
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).afterId, 9_500)
    }

    func testAnEndpointWithNoUrlOrTokenPersistsNothing() throws {
        let store = try MessageStore.open(path: ":memory:")
        XCTAssertEqual(relayCursorKey(relayUrl: url, relayToken: "   "), "")
        XCTAssertEqual(
            try store.advanceRelayFetchCursor(configKey: "", pageNextCursor: 9_000, pageFullyProcessed: true),
            0
        )
        try store.noteRelaySweepCompleted(configKey: "", nowMs: 5_000)
        XCTAssertEqual(try store.relayFetchCursor(configKey: "").afterId, 0)
        XCTAssertEqual(try store.relayFetchCursor(configKey: "").lastSweepAtMs, 0)
    }

    // MARK: - sweep scheduling

    func testAColdStartHonoursTheStoredSweepTimestampInsteadOfReWalking() {
        // A sweep re-downloads the sealed body of every row still in the
        // mailbox. Forcing one per process start made the restart rate -- not
        // the interval -- set the bandwidth bill.
        let sweptAt: Int64 = 1_000_000
        let interval = relaySweepIntervalMs()
        XCTAssertFalse(relaySweepDue(sweptThisSession: false, lastSweepAtMs: sweptAt, sweepProgressAfterId: 0, nowMs: sweptAt))
        XCTAssertFalse(
            relaySweepDue(sweptThisSession: false, lastSweepAtMs: sweptAt, sweepProgressAfterId: 0, nowMs: sweptAt + interval - 1)
        )
        // Stale enough, and a cold start sweeps like any other pass.
        XCTAssertTrue(
            relaySweepDue(sweptThisSession: false, lastSweepAtMs: sweptAt, sweepProgressAfterId: 0, nowMs: sweptAt + interval)
        )
    }

    func testAMailboxNeverSweptSweepsOnceNotOncePerPass() {
        // Fresh install, rotated token, and moved host read as 0 and must walk
        // from the beginning. Restore preserves a recent frontier instead.
        XCTAssertTrue(relaySweepDue(sweptThisSession: false, lastSweepAtMs: 0, sweepProgressAfterId: 0, nowMs: 5_000))
        // ...but a store write that keeps failing must not re-walk forever.
        XCTAssertFalse(relaySweepDue(sweptThisSession: true, lastSweepAtMs: 0, sweepProgressAfterId: 0, nowMs: 5_000))
    }

    func testLaterPassesSweepOnlyOnceTheIntervalHasElapsed() {
        let sweptAt: Int64 = 1_000_000
        let interval = relaySweepIntervalMs()
        XCTAssertEqual(interval, 6 * 60 * 60 * 1000)
        XCTAssertFalse(relaySweepDue(sweptThisSession: true, lastSweepAtMs: sweptAt, sweepProgressAfterId: 0, nowMs: sweptAt))
        XCTAssertFalse(
            relaySweepDue(sweptThisSession: true, lastSweepAtMs: sweptAt, sweepProgressAfterId: 0, nowMs: sweptAt + interval - 1)
        )
        XCTAssertTrue(
            relaySweepDue(sweptThisSession: true, lastSweepAtMs: sweptAt, sweepProgressAfterId: 0, nowMs: sweptAt + interval)
        )
    }

    func testABackwardsClockSweepsRatherThanPinningTheMailbox() {
        XCTAssertTrue(relaySweepDue(sweptThisSession: true, lastSweepAtMs: 5_000_000, sweepProgressAfterId: 0, nowMs: 1_000))
        XCTAssertTrue(relaySweepDue(sweptThisSession: false, lastSweepAtMs: 5_000_000, sweepProgressAfterId: 0, nowMs: 1_000))
        XCTAssertTrue(relaySweepDue(sweptThisSession: false, lastSweepAtMs: Int64.max, sweepProgressAfterId: 0, nowMs: 1_000))
    }

    func testACompletedSweepRestartsTheIntervalWithoutCostingTheFrontier() throws {
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 9_000, pageFullyProcessed: true)
        try store.noteRelaySweepCompleted(configKey: key(), nowMs: 1_000_000)
        let cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(cursor.afterId, 9_000)
        XCTAssertEqual(cursor.lastSweepAtMs, 1_000_000)
        XCTAssertFalse(
            relaySweepDue(sweptThisSession: true, lastSweepAtMs: cursor.lastSweepAtMs, sweepProgressAfterId: cursor.sweepAfterId, nowMs: 1_000_001)
        )
    }

    func testASweepStartsAtZeroAndANormalPassResumesFromTheFrontier() {
        XCTAssertEqual(relayPassStartCursor(sweeping: true, persistedAfterId: 9_000, sweepProgressAfterId: 0), 0)
        XCTAssertEqual(relayPassStartCursor(sweeping: false, persistedAfterId: 9_000, sweepProgressAfterId: 0), 9_000)
        XCTAssertEqual(relayPassStartCursor(sweeping: false, persistedAfterId: -5, sweepProgressAfterId: 0), 0)
    }

    // MARK: - the sweep's own resume cursor

    func testAYieldedSweepResumesFromItsProgressInsteadOfRestartingAtZero() throws {
        // The livelock. A walk is bounded (relayMailboxWalkAction), and a
        // sweep is only recorded complete on the empty page that ends the
        // mailbox. On any mailbox holding more than one budget's worth of
        // hint-matching rows the sweep never reached that page, stayed due,
        // and started again at 0 a second later -- the same first 512 rows
        // re-downloaded every few seconds, indefinitely.
        let store = try MessageStore.open(path: ":memory:")
        // A long-established mailbox: frontier at the top, last swept six
        // hours ago.
        _ = try store.advanceRelayFetchCursor(
            configKey: key(),
            pageNextCursor: 29_000,
            pageFullyProcessed: true
        )
        try store.noteRelaySweepCompleted(configKey: key(), nowMs: 1_000_000)
        let now: Int64 = 1_000_000 + relaySweepIntervalMs()

        var cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertTrue(relaySweepDue(
            sweptThisSession: true,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: now
        ))
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: true,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            0
        )

        // Four pages, then the budget runs out and the pass yields.
        for pageCursor: Int64 in [128, 256, 384, 512] {
            _ = try store.advanceRelaySweepCursor(
                configKey: key(),
                pageNextCursor: pageCursor,
                pageFullyProcessed: true,
                nowMs: 1_000
            )
        }

        cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(cursor.sweepAfterId, 512)
        XCTAssertEqual(cursor.afterId, 29_000)
        // Still due -- an unfinished sweep must be finished, whatever the
        // timestamp says -- and it picks up where it stopped.
        XCTAssertTrue(relaySweepDue(
            sweptThisSession: true,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: now
        ))
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: true,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            512
        )
        // An ordinary pass in between still reads the frontier, never this.
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: false,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            29_000
        )

        // The empty page ends it: interval restarts, resume cursor cleared.
        try store.noteRelaySweepCompleted(configKey: key(), nowMs: now)
        cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(cursor.sweepAfterId, 0)
        XCTAssertEqual(cursor.afterId, 29_000)
        XCTAssertFalse(relaySweepDue(
            sweptThisSession: true,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: now + 1
        ))
    }

    func testSweepProgressObeysTheFrontiersRuleAndNeverSlipsBackwards() throws {
        let store = try MessageStore.open(path: ":memory:")
        XCTAssertEqual(
            try store.advanceRelaySweepCursor(
                configKey: key(),
                pageNextCursor: 256,
                pageFullyProcessed: true,
                nowMs: 1_000
            ),
            256
        )
        // A page that did not reach a terminal disposition for every envelope,
        // or failed to land its acks, must be presented again.
        XCTAssertEqual(
            try store.advanceRelaySweepCursor(
                configKey: key(),
                pageNextCursor: 512,
                pageFullyProcessed: false,
                nowMs: 1_000
            ),
            256
        )
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).sweepAfterId, 256)
        XCTAssertEqual(
            try store.advanceRelaySweepCursor(
                configKey: key(),
                pageNextCursor: 128,
                pageFullyProcessed: true,
                nowMs: 1_000
            ),
            256
        )
        // An endpoint with no url or token persists nothing here either.
        XCTAssertEqual(
            try store.advanceRelaySweepCursor(
                configKey: "",
                pageNextCursor: 512,
                pageFullyProcessed: true,
                nowMs: 1_000
            ),
            0
        )
        XCTAssertEqual(try store.relayFetchCursor(configKey: "").sweepAfterId, 0)
    }

    func testASweepThatSurvivesARestartResumesRatherThanStartingOver() throws {
        // The app is killed and relaunched all day. Before the resume cursor,
        // every restart mid-sweep threw the whole walk away.
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelaySweepCursor(
            configKey: key(),
            pageNextCursor: 512,
            pageFullyProcessed: true,
            nowMs: 1_000
        )
        let cursor = try store.relayFetchCursor(configKey: key())
        // RelaySweepSession is empty again after a restart, but that guard is
        // not what keeps this sweep alive -- the persisted progress is.
        XCTAssertTrue(relaySweepDue(
            sweptThisSession: false,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: 9_999
        ))
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: true,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            512
        )
    }

    func testASweepStalledAcrossDaysOfflineWalksFromZeroAgain() throws {
        // The rebuilt-relay case. A phone goes offline mid-sweep for days;
        // while it is away the relay is rebuilt from a fresh volume and its row
        // ids restart at 1. The remembered resume cursor now points past the
        // end of the mailbox, so resuming from it would fetch one empty page,
        // record a sweep that covered nothing at all, and put the mailbox back
        // to sleep for another interval while real mail sat below a frontier no
        // ordinary pass goes under.
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(
            configKey: key(),
            pageNextCursor: 29_000,
            pageFullyProcessed: true
        )
        try store.noteRelaySweepCompleted(configKey: key(), nowMs: 1_000_000)
        let sweepStarted: Int64 = 1_000_000 + relaySweepIntervalMs()
        _ = try store.advanceRelaySweepCursor(
            configKey: key(),
            pageNextCursor: 20_000,
            pageFullyProcessed: true,
            nowMs: sweepStarted
        )

        let backOnline: Int64 = sweepStarted + 3 * 24 * 60 * 60 * 1000
        var cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertTrue(relaySweepDue(
            sweptThisSession: false,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: backOnline
        ))
        XCTAssertTrue(relaySweepRestartFromZero(
            sweepProgressAfterId: cursor.sweepAfterId,
            sweepStartedAtMs: cursor.sweepStartedAtMs,
            nowMs: backOnline
        ))

        try store.resetRelaySweepProgress(configKey: key(), nowMs: backOnline)
        cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: true,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            0
        )
        // The frontier is not what proved wrong, so it is left alone.
        XCTAssertEqual(cursor.afterId, 29_000)

        // ...and the walk that starts here is dated, so the pass a second later
        // resumes it rather than restarting it again -- otherwise the repair
        // would be its own re-download loop.
        _ = try store.advanceRelaySweepCursor(
            configKey: key(),
            pageNextCursor: 512,
            pageFullyProcessed: true,
            nowMs: backOnline
        )
        cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertFalse(relaySweepRestartFromZero(
            sweepProgressAfterId: cursor.sweepAfterId,
            sweepStartedAtMs: cursor.sweepStartedAtMs,
            nowMs: backOnline + relayMailboxContinuationDelayMs()
        ))
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: true,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            512
        )
    }

    func testASweepThatYieldedMomentsAgoResumesRatherThanRestarting() throws {
        // Why this is a staleness question rather than an empty-page one: a
        // walk yields on a fixed budget, so about one sweep in four yields
        // exactly at the end of the mailbox and then resumes into a perfectly
        // honest empty page. Re-walking that from 0 would land on the same
        // boundary again -- the same loop, slower.
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelaySweepCursor(
            configKey: key(),
            pageNextCursor: 512,
            pageFullyProcessed: true,
            nowMs: 5_000_000
        )
        let cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(cursor.sweepStartedAtMs, 5_000_000)
        XCTAssertFalse(relaySweepRestartFromZero(
            sweepProgressAfterId: cursor.sweepAfterId,
            sweepStartedAtMs: cursor.sweepStartedAtMs,
            nowMs: 5_001_000
        ))
    }

    func testAbandoningAWalkHandsTheMailboxBackToTheSchedule() throws {
        // A relay that answers incoherently -- rows returned, cursor standing
        // still -- ends the walk without completing the sweep. The progress it
        // leaves behind has to go: a mailbox that reads as "a sweep is under
        // way" on every pass never runs an ordinary frontier pass again, so new
        // mail at the top of it would stop arriving altogether.
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(
            configKey: key(),
            pageNextCursor: 29_000,
            pageFullyProcessed: true
        )
        _ = try store.advanceRelaySweepCursor(
            configKey: key(),
            pageNextCursor: 512,
            pageFullyProcessed: true,
            nowMs: 1_000
        )
        var cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertTrue(relaySweepDue(
            sweptThisSession: true,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: 2_000
        ))

        try store.resetRelaySweepProgress(configKey: key(), nowMs: 2_000)
        cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(cursor.sweepAfterId, 0)
        XCTAssertFalse(relaySweepDue(
            sweptThisSession: true,
            lastSweepAtMs: cursor.lastSweepAtMs,
            sweepProgressAfterId: cursor.sweepAfterId,
            nowMs: 2_000
        ))
        XCTAssertEqual(
            relayPassStartCursor(
                sweeping: false,
                persistedAfterId: cursor.afterId,
                sweepProgressAfterId: cursor.sweepAfterId
            ),
            29_000
        )
    }

    // MARK: - the per-pass walk budget

    func testTheWalkYieldsOnceItRunsOutOfPagesOrEnvelopes() {
        // iOS had no budget at all before this: the walk was a `while true`
        // that ran until the mailbox emptied, so one deep mailbox could hold a
        // whole sync pass and every mailbox queued behind it.
        XCTAssertEqual(relayMailboxWalkAction(pagesFetched: 0, envelopesFetched: 0), .continueWalk)
        XCTAssertEqual(
            relayMailboxWalkAction(
                pagesFetched: relayMailboxMaxPagesPerPass() - 1,
                envelopesFetched: 12
            ),
            .continueWalk
        )
        XCTAssertEqual(
            relayMailboxWalkAction(
                pagesFetched: relayMailboxMaxPagesPerPass(),
                envelopesFetched: 12
            ),
            .yieldAndScheduleContinuation
        )
        // Either budget alone ends the pass: relayd fills a page to a byte
        // cap, so two pages can be worth more work than four.
        XCTAssertEqual(
            relayMailboxWalkAction(
                pagesFetched: 2,
                envelopesFetched: relayMailboxMaxEnvelopesPerPass()
            ),
            .yieldAndScheduleContinuation
        )
        XCTAssertEqual(
            relayMailboxWalkAction(
                pagesFetched: 2,
                envelopesFetched: relayMailboxMaxEnvelopesPerPass() - 1
            ),
            .continueWalk
        )
    }

    func testBothShellsReadTheSameBudgetFromTheCore() {
        XCTAssertEqual(relayMailboxMaxPagesPerPass(), 4)
        XCTAssertEqual(relayMailboxMaxEnvelopesPerPass(), 512)
        XCTAssertEqual(relayMailboxContinuationDelayMs(), 1_000)
    }

    func testClearingEveryCursorMakesTheNextPassReWalk() throws {
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 9_000, pageFullyProcessed: true)
        try store.clearRelayFetchCursors()
        XCTAssertEqual(try store.relayFetchCursor(configKey: key()).afterId, 0)
    }

    func testTheSweepSessionRemembersOnlyWhatCompleted() {
        let session = RelaySweepSession()
        XCTAssertFalse(session.hasSwept(key()))
        session.noteSwept(key())
        XCTAssertTrue(session.hasSwept(key()))
        XCTAssertFalse(session.hasSwept(relayCursorKey(relayUrl: url, relayToken: "other")))
        session.reset()
        XCTAssertFalse(session.hasSwept(key()))
    }

    // MARK: - batch limit and walk termination

    func testTheFetchBatchLimitIsTheRaisedOneAndThePathBuilderAcceptsIt() throws {
        let limit = relayFetchBatchLimit()
        XCTAssertEqual(limit, 256)
        // relayd's own MAX_FETCH_LIMIT is 500, so the deployed server takes
        // this without clamping.
        XCTAssertLessThanOrEqual(Int(limit), 500)
        let path = try relayBuildFetchPath(hints: [Data(repeating: 2, count: 8)], afterId: 0, limit: limit)
        XCTAssertTrue(path.contains("limit=256"))
    }

    func testAServerThatClampsTheLimitDoesNotEndTheWalkEarly() {
        // We ask for 256 and a server hands back 50. Treating a short page as
        // end-of-mailbox would strand every row above it -- in an ascending-id
        // mailbox, all the new mail.
        XCTAssertTrue(relayFetchWalkContinues(pageEnvelopeCount: 50, afterId: 0, pageNextCursor: 50))
        XCTAssertTrue(relayFetchWalkContinues(pageEnvelopeCount: 1, afterId: 0, pageNextCursor: 1))
    }

    func testOnlyAnEmptyPageEndsTheWalk() {
        XCTAssertFalse(relayFetchWalkContinues(pageEnvelopeCount: 0, afterId: 100, pageNextCursor: 100))
        XCTAssertTrue(relayFetchWalkContinues(pageEnvelopeCount: 256, afterId: 100, pageNextCursor: 356))
    }

    func testAPageTruncatedByTheServersByteBudgetKeepsTheWalkGoing() {
        // relayd stops filling a page once its cumulative sealed bytes would
        // push the response past what this client will decode, so a mailbox
        // of large attachment chunks answers a 256-row ask with a handful of
        // rows every time. Reading that as end-of-mailbox would strand the
        // newest mail, which in an ascending-id mailbox has the highest ids.
        XCTAssertTrue(relayFetchWalkContinues(pageEnvelopeCount: 12, afterId: 0, pageNextCursor: 12))
        XCTAssertTrue(relayFetchWalkContinues(pageEnvelopeCount: 9, afterId: 12, pageNextCursor: 21))
        XCTAssertTrue(relayFetchWalkContinues(pageEnvelopeCount: 1, afterId: 21, pageNextCursor: 22))
        XCTAssertFalse(relayFetchWalkContinues(pageEnvelopeCount: 0, afterId: 22, pageNextCursor: 22))
    }

    func testAnOversizePageHalvesTheAskDownToOneRowAndThenStops() {
        var limit = relayFetchBatchLimit()
        var ladder: [UInt32] = [limit]
        while let next = relayFetchShrunkLimit(currentLimit: limit) {
            limit = next
            ladder.append(limit)
        }
        XCTAssertEqual(ladder, [256, 128, 64, 32, 16, 8, 4, 2, 1])
        // One row is the floor: nothing smaller exists to ask for.
        XCTAssertNil(relayFetchShrunkLimit(currentLimit: 1))
    }

    func testACursorThatDoesNotAdvanceEndsTheWalkInsteadOfLooping() {
        XCTAssertFalse(relayFetchWalkContinues(pageEnvelopeCount: 16, afterId: 100, pageNextCursor: 100))
        XCTAssertFalse(relayFetchWalkContinues(pageEnvelopeCount: 16, afterId: 100, pageNextCursor: 99))
    }

    // MARK: - the doorbell subscribes from the frontier too

    func testThePushSubscribeUrlCarriesTheFrontierInsteadOfAHardcodedZero() throws {
        // relayd replays from `after` on every reconnect. At 0 that is the
        // entire mailbox, serialized into frames this client discards one by
        // one -- pure server load and bandwidth for no behavior change.
        let config = RelayConfig(relayUrl: url, relayToken: token)
        let built = RelayPushClient.buildWebSocketURL(
            config: config,
            hints: [Data(repeating: 2, count: 8)],
            afterId: 28_800
        )
        let absolute = try XCTUnwrap(built?.absoluteString)
        XCTAssertTrue(absolute.hasPrefix("wss://relay.example/ws?hints="))
        XCTAssertTrue(absolute.hasSuffix("&after=28800"))
    }

    func testANegativePushCursorIsClampedRatherThanRejectedByTheRelay() throws {
        let config = RelayConfig(relayUrl: url, relayToken: token)
        let built = RelayPushClient.buildWebSocketURL(
            config: config,
            hints: [Data(repeating: 2, count: 8)],
            afterId: -1
        )
        XCTAssertTrue(try XCTUnwrap(built?.absoluteString).hasSuffix("&after=0"))
    }

    func testANonHttpsRelayStillBuildsNoPushUrl() {
        // Unchanged by the cursor plumbing: the core refuses the URL, and a
        // relative "/ws?..." is not something a socket can open.
        let config = RelayConfig(relayUrl: "http://relay.example", relayToken: token)
        XCTAssertNil(RelayPushClient.buildWebSocketURL(
            config: config,
            hints: [Data(repeating: 2, count: 8)],
            afterId: 0
        ))
    }
}
