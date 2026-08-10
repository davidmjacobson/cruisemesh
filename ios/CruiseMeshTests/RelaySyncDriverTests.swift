import XCTest
@testable import CruiseMesh

/// What the driver is allowed to decide, and what it must not — plus a whole
/// core pass driven end to end by the code that will drive it on a phone.
///
/// The executor cases are one clause of the seam each: the ids it echoes, the
/// cap it enforces, the headers it drops, the failures it names, and the one
/// thing it is emphatically not allowed to do — interpret a status code. The
/// pass cases prove `CoreRelayPass`, `RelaySyncDriver` and `RelayActionDriver`
/// compose into something that terminates, posts what is queued, retires what
/// the relay accepted, and leaves the queue alone when it does not. Mirrors the
/// Android `CoreRelayDriverTest` and `CoreRelayPassRunnerTest`.
final class RelaySyncDriverTests: XCTestCase {

    private static let now: Int64 = 1_700_000_000_000
    private var previousSession: URLSession!

    override func setUp() {
        super.setUp()
        previousSession = RelayClient.urlSession
        CoreRelayFakeURLProtocol.reset()
        RelayClient.urlSession = CoreRelayFakeURLProtocol.makeSession()
    }

    override func tearDown() {
        RelayClient.urlSession = previousSession
        CoreRelayFakeURLProtocol.reset()
        super.tearDown()
    }

    // MARK: - the driver, one action at a time

    func testAResultEchoesTheIdsItWasHanded() {
        CoreRelayFakeURLProtocol.handler = { _, _ in (200, [:], Data(#"{"id":7}"#.utf8)) }
        let result = RelayActionDriver.execute(
            passId: "pass-7", actionId: 42, request: postRequest(), nowMs: Self.now
        )
        XCTAssertEqual(result.passId, "pass-7")
        XCTAssertEqual(result.actionId, 42)
        XCTAssertEqual(result.completedAtMs, Self.now)
        XCTAssertEqual(result.status, 200)
        XCTAssertNil(result.error)
    }

    func testOnlyTheResponseHeadersCoreAskedForComeBack() {
        CoreRelayFakeURLProtocol.handler = { _, _ in
            (429, ["Retry-After": "30", "X-Relay-Node": "node-4"], Data(#"{"code":"rate_limited"}"#.utf8))
        }
        let result = RelayActionDriver.execute(
            passId: "p", actionId: 1, request: postRequest(), nowMs: Self.now
        )
        XCTAssertEqual(result.headers, [CoreRelayHeader(name: "Retry-After", value: "30")])
    }

    func testAFailingStatusIsAStatusNotAFailureToReachTheRelay() {
        CoreRelayFakeURLProtocol.handler = { _, _ in (507, [:], Data(#"{"code":"mailbox_full"}"#.utf8)) }
        let result = RelayActionDriver.execute(
            passId: "p", actionId: 1, request: postRequest(), nowMs: Self.now
        )
        // The driver classifies nothing. Core reads the status and the relay's
        // own code out of the body and decides what they mean.
        XCTAssertEqual(result.status, 507)
        XCTAssertNil(result.error)
        XCTAssertTrue(String(data: result.body, encoding: .utf8)?.contains("mailbox_full") == true)
    }

    func testABodyPastTheDeclaredCapIsRefusedRatherThanAccumulated() {
        CoreRelayFakeURLProtocol.handler = { _, _ in (200, [:], Data(String(repeating: "x", count: 4_096).utf8)) }
        var request = postRequest()
        request.maxResponseBytes = 512
        let result = RelayActionDriver.execute(passId: "p", actionId: 1, request: request, nowMs: Self.now)
        XCTAssertEqual(result.error, .bodyTooLarge)
        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(result.body, Data())
    }

    func testAnOversizedErrorPageIsStillAStatusNotAnOversizedPage() {
        // A captive portal or a proxy banner. Calling this an oversized page
        // sends a fetch down the shrink ladder and discards the Retry-After a
        // rate limit would have carried.
        CoreRelayFakeURLProtocol.handler = { _, _ in
            (502, ["Retry-After": "5"], Data(("<html>" + String(repeating: "y", count: 8_192) + "</html>").utf8))
        }
        var request = postRequest()
        request.maxResponseBytes = 512
        let result = RelayActionDriver.execute(passId: "p", actionId: 1, request: request, nowMs: Self.now)
        XCTAssertEqual(result.status, 502)
        XCTAssertNil(result.error)
        XCTAssertEqual(result.headers, [CoreRelayHeader(name: "Retry-After", value: "5")])
    }

    func testAConnectionThatIsNeverAnsweredIsATransportFailureNotAStatus() {
        CoreRelayFakeURLProtocol.handler = nil // no answer at all
        let result = RelayActionDriver.execute(passId: "p", actionId: 1, request: postRequest(), nowMs: Self.now)
        XCTAssertEqual(result.status, 0)
        XCTAssertTrue(
            result.error == .connectionFailed || result.error == .timeout,
            "an unanswered connection must map to a transport error, got \(String(describing: result.error))"
        )
        XCTAssertEqual(result.completedAtMs, Self.now)
    }

    func testACancelledDriverSaysSoInsteadOfReportingAnOutage() {
        CoreRelayFakeURLProtocol.handler = { _, _ in (200, [:], Data("{}".utf8)) }
        let result = RelayActionDriver.execute(
            passId: "p", actionId: 1, request: postRequest(), nowMs: Self.now, isCancelled: { true }
        )
        XCTAssertEqual(result.error, .cancelled)
        // Nothing was sent: a cancellation checked before the request means the
        // relay never heard from this pass at all.
        XCTAssertEqual(result.status, 0)
        XCTAssertTrue(CoreRelayFakeURLProtocol.recorded.isEmpty)
    }

    func testAGetCarriesNoBodyAndAPostCarriesExactlyTheBytesCoreFormed() {
        CoreRelayFakeURLProtocol.handler = { _, _ in (200, [:], Data("{}".utf8)) }
        _ = RelayActionDriver.execute(passId: "p", actionId: 1, request: postRequest(), nowMs: Self.now)
        XCTAssertEqual(CoreRelayFakeURLProtocol.recorded[0].body ?? Data(), postRequest().body)

        var fetch = postRequest()
        fetch.operation = .fetchPage
        fetch.method = "GET"
        fetch.path = "/envelopes?after=0&limit=8"
        fetch.headers = [CoreRelayHeader(name: "Authorization", value: "Bearer member-token")]
        fetch.body = Data()
        _ = RelayActionDriver.execute(passId: "p", actionId: 2, request: fetch, nowMs: Self.now)
        let recorded = CoreRelayFakeURLProtocol.recorded[1]
        XCTAssertEqual(recorded.request.httpMethod, "GET")
        XCTAssertEqual(recorded.request.url?.path, "/envelopes")
        XCTAssertEqual(recorded.request.url?.query, "after=0&limit=8")
        XCTAssertNil(recorded.body)
    }

    // MARK: - a whole pass

    func testAFullPassPostsTheQueueRetiresWhatTheRelayTookAndFinishes() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()
        try fixture.queueAuthored(3)

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .completed)
        XCTAssertEqual(summary.authoredUploads, 3)
        XCTAssertEqual(relay.posts, 3)
        XCTAssertEqual(try fixture.pendingAuthored(), 0)
    }

    func testARelayThatRefusesTheMailboxLeavesEveryRowQueued() throws {
        let relay = CoreRelayFakeRelay()
        relay.postResponse = { (507, [:], Data(#"{"code":"mailbox_full"}"#.utf8)) }
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()
        try fixture.queueAuthored(3)

        let summary = fixture.run()

        XCTAssertEqual(summary.authoredUploads, 0)
        XCTAssertEqual(try fixture.pendingAuthored(), 3)
        // The lane stopped spending on a mailbox that said it was full, rather
        // than offering it every remaining row.
        XCTAssertEqual(relay.posts, 1)
    }

    func testTheFirstFamilyRateLimitEndsThePassAndReportsTheWindowItEarned() throws {
        let relay = CoreRelayFakeRelay()
        relay.postResponse = { (429, ["Retry-After": "30"], Data(#"{"code":"rate_limited"}"#.utf8)) }
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()
        try fixture.queueAuthored(4)

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .rateLimited)
        XCTAssertEqual(relay.posts, 1)
        XCTAssertGreaterThanOrEqual(summary.quietUntilMs, Self.now + 30_000)
        // Nothing was retired: a refusal is not a delivery.
        XCTAssertEqual(try fixture.pendingAuthored(), 4)
    }

    func testACancelledPassStopsAskingAndAdmitsIt() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture(cancelled: true)
        try fixture.queueAuthored(2)

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .cancelled)
        XCTAssertEqual(relay.posts, 0)
        XCTAssertEqual(try fixture.pendingAuthored(), 2)
    }

    func testAPassStartedInsideAQuietWindowSpendsNothing() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture(quietUntilMs: Self.now + 60_000)
        try fixture.queueAuthored(2)

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .refusedQuietWindow)
        XCTAssertEqual(relay.posts, 0)
    }

    func testARelayThatCannotBeReachedAtAllCostsTheQueueNothing() throws {
        // No handler installed: every request is a transport failure and none
        // may retire a row.
        RelayClient.urlSession = CoreRelayFakeURLProtocol.makeSession()
        CoreRelayFakeURLProtocol.handler = nil
        let fixture = try Fixture()
        try fixture.queueAuthored(2)

        let summary = fixture.run()

        XCTAssertEqual(summary.authoredUploads, 0)
        XCTAssertEqual(try fixture.pendingAuthored(), 2)
        XCTAssertTrue(
            summary.outcome != .completed || summary.requestsIssued > 0,
            "a pass that reached nothing must not report itself healthy"
        )
    }

    func testAWalkFetchesAPageAcksWhatItConsumedAndMovesTheFrontier() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture(hinted: true)
        relay.fetchBody = try fixture.consumedPage(ids: [3, 5, 8])

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .completed)
        XCTAssertFalse(relay.fetchPaths.isEmpty, "the walk must have fetched")
        let first = try XCTUnwrap(relay.fetchPaths.first)
        XCTAssertTrue(first.hasPrefix("/envelopes?hints="), "a fetch must carry its hints, got \(first)")
        XCTAssertTrue(first.contains("&after=0&limit="), "a fetch must carry its cursor, got \(first)")
        XCTAssertEqual(relay.acks, 1, "the page's rows must earn exactly one ack")
        XCTAssertTrue(relay.ackBody.contains("[3,5,8]"), "the ack must name the ids the page carried, got \(relay.ackBody)")
        XCTAssertEqual(summary.rowsAcked, 3)
        XCTAssertEqual(try fixture.frontier(), 8)
    }

    func testARateLimitOnAFetchIsReadFromTheHeaderThatFetchAnsweredWith() throws {
        let relay = CoreRelayFakeRelay()
        relay.fetchResponse = { (429, ["Retry-After": "45"], Data(#"{"code":"rate_limited"}"#.utf8)) }
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture(hinted: true)

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .rateLimited)
        XCTAssertGreaterThanOrEqual(summary.quietUntilMs, Self.now + 45_000)
    }

    func testTheRunnerNeverLetsASessionSpin() throws {
        // A driver result naming a pass the session does not recognise is
        // ignored by IDEMP-01, so the session re-states its action. Left alone
        // that is an infinite loop between two correct components; the guard
        // turns it into a bounded failure.
        let store = try MessageStore.open(path: ":memory:")
        var issued = 0
        let executor = ScriptedExecutor { _, actionId, _, nowMs in
            issued += 1
            return CoreRelayHttpResult(
                passId: "not-this-pass",
                actionId: 999,
                status: 0,
                headers: [],
                body: Data(),
                error: .other,
                completedAtMs: nowMs
            )
        }
        let plan = CoreRelayPassPlan(
            own: CoreRelayEndpointConfig(url: "https://relay.test", token: "t"),
            contacts: [],
            ownUserId: Data(repeating: 1, count: 32),
            fetchHints: [Data(repeating: 2, count: 8)],
            presenceAnnounce: [],
            presenceQuery: [],
            ownEndpointChanged: false,
            sweptThisSession: true,
            consecutiveRateLimits: 0,
            quietUntilMs: 0,
            budgets: coreRelayPassDefaultBudgets()
        )
        let summary = RelaySyncDriver(store: store, executor: executor, clock: { Self.now })
            .run(plan: plan, passId: "g")
        XCTAssertEqual(summary.outcome, .cancelled)
        XCTAssertTrue((1...1_000).contains(issued), "the guard must bound the loop, saw \(issued)")
    }

    // MARK: - helpers

    private func postRequest() -> CoreRelayHttpRequest {
        CoreRelayHttpRequest(
            operation: .postEnvelope,
            method: "POST",
            baseUrl: "https://relay.test",
            path: "/envelopes",
            headers: [
                CoreRelayHeader(name: "Authorization", value: "Bearer member-token"),
                CoreRelayHeader(name: "Content-Type", value: "application/json"),
                CoreRelayHeader(name: "Accept", value: "application/json"),
            ],
            body: Data(#"{"msg_id":"EREREREREREREREREREREQ"}"#.utf8),
            maxResponseBytes: 65_536,
            responseHeadersWanted: ["Retry-After"]
        )
    }

    /// Executes actions from a closure — a scripted relay the runner cannot
    /// tell from the real driver.
    private struct ScriptedExecutor: RelayActionExecutor {
        let script: (String, UInt64, CoreRelayHttpRequest, Int64) -> CoreRelayHttpResult
        func execute(passId: String, actionId: UInt64, request: CoreRelayHttpRequest, nowMs: Int64) -> CoreRelayHttpResult {
            script(passId, actionId, request, nowMs)
        }
    }

    /// A store with an identity, a contact and this device's own pass, driven
    /// through the real `RelayActionDriver` against `CoreRelayFakeURLProtocol`.
    private final class Fixture {
        private let baseUrl = "https://relay.test"
        private let cancelled: Bool
        private let quietUntilMs: Int64
        private let hinted: Bool
        private let identity = generateIdentity()
        private let peer = generateIdentity()
        private let store: MessageStore
        private let contact: Contact

        init(cancelled: Bool = false, quietUntilMs: Int64 = 0, hinted: Bool = false) throws {
            self.cancelled = cancelled
            self.quietUntilMs = quietUntilMs
            self.hinted = hinted
            store = try MessageStore.open(path: ":memory:")
            contact = Contact(
                userId: peer.userId,
                name: "Peer",
                signPk: peer.signPk,
                agreePk: peer.agreePk,
                relayUrl: nil,
                relayToken: nil,
                nickname: nil
            )
            try store.upsertContact(contact: contact)
        }

        private func ownHint() -> Data { computeRecipientHint(recipientUserId: identity.userId, timestampMs: now) }

        func consumedPage(ids: [Int64]) throws -> String {
            let hint = ownHint()
            let expiry = now + 6 * 24 * 60 * 60 * 1000
            let rows = try ids.map { id -> String in
                var msgId = Data(repeating: 0, count: 16)
                msgId[0] = UInt8(truncatingIfNeeded: id)
                msgId[8] = 0xA5
                let sealed = Data(repeating: UInt8(truncatingIfNeeded: id), count: 96)
                let recorded = try store.coreRecordConsumedHiddenMsgId(
                    msgId: msgId,
                    kind: ProtocolKind.receipt,
                    recipientHint: hint,
                    expiryMs: expiry,
                    ownUserId: identity.userId,
                    nowMs: now
                )
                XCTAssertTrue(recorded, "the consumed set must vouch for the seeded row")
                return "{\"id\":\(id),\"msg_id\":\"\(relayBase64Url(msgId))\",\"hop_ttl\":3,"
                    + "\"recipient_hint\":\"\(relayBase64Url(hint))\","
                    + "\"sealed\":\"\(relayBase64Url(sealed))\",\"expiry_ms\":\(expiry)}"
            }.joined(separator: ",")
            return "{\"envelopes\":[\(rows)],\"next_cursor\":\(ids.last ?? 0)}"
        }

        func frontier() throws -> Int64 {
            try store.relayFetchCursor(configKey: relayCursorKey(relayUrl: baseUrl, relayToken: "member-token")).afterId
        }

        func queueAuthored(_ count: Int) throws {
            for index in 0..<count {
                _ = try store.authorPairwiseMessage(
                    identity: identity,
                    contact: contact,
                    kind: 1,
                    payload: Data("row-\(index)".utf8),
                    replyToMsgId: nil,
                    timestampMs: now
                )
            }
        }

        func pendingAuthored() throws -> Int {
            try store.pendingRelayOutboundEnvelopes(limit: 64, nowMs: now, skipRecipientUserIds: []).count
        }

        func run() -> CoreRelayPassSummary {
            let executor = LiveRelayActionExecutor(isCancelled: { self.cancelled })
            let plan = CoreRelayPassPlan(
                own: CoreRelayEndpointConfig(url: baseUrl, token: "member-token"),
                contacts: [],
                ownUserId: identity.userId,
                fetchHints: hinted ? [ownHint()] : [],
                presenceAnnounce: [],
                presenceQuery: [],
                ownEndpointChanged: false,
                sweptThisSession: true,
                consecutiveRateLimits: 0,
                quietUntilMs: quietUntilMs,
                budgets: coreRelayPassDefaultBudgets()
            )
            return RelaySyncDriver(
                store: store,
                executor: executor,
                clock: { now },
                isCancelled: { self.cancelled }
            ).run(plan: plan, passId: "t")
        }

        private var now: Int64 { RelaySyncDriverTests.now }
    }
}
