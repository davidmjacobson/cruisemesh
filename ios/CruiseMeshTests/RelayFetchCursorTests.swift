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

    func testTheFirstPassOfAProcessSweepsWhateverTheStoredTimestampSays() {
        XCTAssertTrue(relaySweepDue(sweptThisSession: false, lastSweepAtMs: 0, nowMs: 1_000))
        XCTAssertTrue(relaySweepDue(sweptThisSession: false, lastSweepAtMs: 1_000, nowMs: 1_000))
        XCTAssertTrue(relaySweepDue(sweptThisSession: false, lastSweepAtMs: Int64.max, nowMs: 1_000))
    }

    func testLaterPassesSweepOnlyOnceTheIntervalHasElapsed() {
        let sweptAt: Int64 = 1_000_000
        let interval = relaySweepIntervalMs()
        XCTAssertEqual(interval, 6 * 60 * 60 * 1000)
        XCTAssertFalse(relaySweepDue(sweptThisSession: true, lastSweepAtMs: sweptAt, nowMs: sweptAt))
        XCTAssertFalse(
            relaySweepDue(sweptThisSession: true, lastSweepAtMs: sweptAt, nowMs: sweptAt + interval - 1)
        )
        XCTAssertTrue(
            relaySweepDue(sweptThisSession: true, lastSweepAtMs: sweptAt, nowMs: sweptAt + interval)
        )
    }

    func testABackwardsClockSweepsRatherThanPinningTheMailbox() {
        XCTAssertTrue(relaySweepDue(sweptThisSession: true, lastSweepAtMs: 5_000_000, nowMs: 1_000))
        XCTAssertTrue(relaySweepDue(sweptThisSession: true, lastSweepAtMs: 0, nowMs: 5_000))
    }

    func testACompletedSweepRestartsTheIntervalWithoutCostingTheFrontier() throws {
        let store = try MessageStore.open(path: ":memory:")
        _ = try store.advanceRelayFetchCursor(configKey: key(), pageNextCursor: 9_000, pageFullyProcessed: true)
        try store.noteRelaySweepCompleted(configKey: key(), nowMs: 1_000_000)
        let cursor = try store.relayFetchCursor(configKey: key())
        XCTAssertEqual(cursor.afterId, 9_000)
        XCTAssertEqual(cursor.lastSweepAtMs, 1_000_000)
        XCTAssertFalse(
            relaySweepDue(sweptThisSession: true, lastSweepAtMs: cursor.lastSweepAtMs, nowMs: 1_000_001)
        )
    }

    func testASweepStartsAtZeroAndANormalPassResumesFromTheFrontier() {
        XCTAssertEqual(relayPassStartCursor(sweeping: true, persistedAfterId: 9_000), 0)
        XCTAssertEqual(relayPassStartCursor(sweeping: false, persistedAfterId: 9_000), 9_000)
        XCTAssertEqual(relayPassStartCursor(sweeping: false, persistedAfterId: -5), 0)
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
