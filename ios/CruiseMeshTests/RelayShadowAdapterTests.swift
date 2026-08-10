import XCTest
@testable import CruiseMesh

/// The canary's safety properties, checked as properties rather than trusted as
/// intentions. Mirrors Android `RelayShadowAdapterTest`.
///
/// Several tests are structural: they assert the *shape* of what the shadow is
/// made of, because "it performs no network I/O" and "it writes nothing
/// operational" are claims a future edit could quietly break while every
/// behavioural test kept passing.
final class RelayShadowAdapterTests: XCTestCase {

    private static let now: Int64 = 1_700_000_000_000
    private static let urlA = "https://relay.example"
    private static let tokenA = "member-token"
    private static let urlB = "https://other.example"
    private static let tokenB = "other-token"
    private static let recipient = Data(repeating: 0x09, count: 32)
    private static let sealedLen = 48

    // MARK: - structural: no second request and no second write are expressible

    func testNothingTheCaptureHoldsCouldOpenAConnection() {
        // If every stored value is a number, a byte array, a string, an enum or
        // a collection of those, there is no object in the shadow's reach with a
        // network method to call. The one closure it carries is the sampling
        // arm, which answers a boolean.
        let capture = RelayShadowPassCapture { true }
        for child in Mirror(reflecting: capture).children {
            if child.label == "armSample" { continue }
            let typeName = String(describing: type(of: child.value))
            XCTAssertFalse(
                forbiddenTypeSubstrings.contains { typeName.contains($0) },
                "RelayShadowPassCapture.\(child.label ?? "?") is a \(typeName); the capture may hold only values"
            )
        }
    }

    func testTheAdapterCannotReachANetworkTypeOrAProductionWrite() throws {
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store)
        var sawSink = false
        for child in Mirror(reflecting: adapter).children {
            XCTAssertFalse(child.value is MessageStore, "the adapter must not hold the message store")
            if child.value is RelayShadowReportSink { sawSink = true }
            let typeName = String(describing: type(of: child.value))
            XCTAssertFalse(
                forbiddenTypeSubstrings.contains { typeName.contains($0) },
                "RelayShadowAdapter.\(child.label ?? "?") is a \(typeName); it must not be a networking or store type"
            )
        }
        XCTAssertTrue(sawSink, "the adapter's one operational collaborator is the diagnostics sink")
    }

    // MARK: - behavioural

    func testTheCoreEngineGetsNoCaptureAtAll() throws {
        let adapter = adapterFor(try MessageStore.open(path: ":memory:"), engine: .core)
        XCTAssertNil(adapter.beginPass(nowMs: Self.now))
    }

    func testWithTheCanaryOffALegacyPassIsUntouched() throws {
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store, shadowEnabled: false)
        for step in 0..<50 {
            XCTAssertNil(adapter.beginPass(nowMs: Self.now + Int64(step) * 3_600_000))
        }
        adapter.finishPass(capture: nil, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: Self.now)
        XCTAssertFalse(try store.exportProtocolEventsJsonl().contains("shadow"))
    }

    func testWithTheCanaryOnAndNothingToCompareALegacyPassIsStillUntouched() throws {
        // The shipping default. A poll tick with an empty outbound and receipt
        // queue is the common relay pass and must cost nothing: no record, no
        // sample spent.
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store)
        for step in 0..<50 {
            let capture = adapter.beginPass(nowMs: Self.now + Int64(step) * 3_600_000)
            capture?.noteSkippedRecipients([Self.recipient])
            capture?.noteUnshadowed(40)
            adapter.finishPass(capture: capture, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: Self.now)
        }
        XCTAssertFalse(try store.exportProtocolEventsJsonl().contains("shadow"))
    }

    func testRowsTheCanaryCannotSpeakForAreCountedEvenWhenTheyCameFirst() throws {
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store)
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        capture.noteUnshadowed(12)
        capture.noteSucceeded(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA))
        adapter.finishPass(capture: capture, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: Self.now)
        XCTAssertTrue(try store.exportProtocolEventsJsonl().contains("\"rows_unshadowed\":12"))
    }

    func testSamplingIsBoundedRatherThanEveryPass() throws {
        let adapter = adapterFor(try MessageStore.open(path: ":memory:"))
        XCTAssertTrue(sampledWithRow(adapter, Self.now), "the first pass with a row is sampled")
        // A burst inside a second must cost one sample, not three.
        XCTAssertFalse(sampledWithRow(adapter, Self.now + 100))
        XCTAssertFalse(sampledWithRow(adapter, Self.now + 200))
    }

    func testTheDailyBoundSurvivesAServiceRestart() {
        // The sampler lives outside the adapter for exactly this: a bound held
        // only in memory resets on every process launch.
        let box = SamplerBox()
        var sampled = 0
        let midnight = Self.now / 86_400_000 * 86_400_000
        for step in 0..<95 {
            // A new adapter each time: this *is* the restarted service.
            let adapter = RelayShadowAdapter(
                sink: RelayShadowReportSink { _, _ in },
                passEngine: { .legacy },
                shadowEnabled: { true },
                loadSampler: { box.state },
                saveSampler: { box.state = $0 }
            )
            if sampledWithRow(adapter, midnight + Int64(step) * 900_000) { sampled += 1 }
        }
        XCTAssertLessThanOrEqual(sampled, 12, "a restarting service must not sample every pass, got \(sampled)")
    }

    func testAnAgreeingPassRecordsThatItRanAndFindsNothing() throws {
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store)
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        capture.noteSucceeded(lane: .receipt, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA))
        adapter.finishPass(capture: capture, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: Self.now)

        let archive = try store.exportProtocolEventsJsonl()
        XCTAssertTrue(archive.contains("\"outcome\":\"shadow_agreed\""))
        XCTAssertFalse(archive.contains(Self.tokenA))
        XCTAssertFalse(archive.contains(Self.urlA))
    }

    func testAMailboxFaultOnlyOneEngineKeepsSpendingOnIsReported() throws {
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store)
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        // A full mailbox is evidence about the mailbox, so core stops spending
        // on it; the legacy engine here offered it the next row anyway.
        capture.noteFailed(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), error: RelayHTTPError(statusCode: 507, relayCode: "mailbox_full", responseBody: "full"))
        capture.noteSucceeded(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA))
        adapter.finishPass(capture: capture, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: Self.now)

        let archive = try store.exportProtocolEventsJsonl()
        XCTAssertTrue(archive.contains("\"outcome\":\"shadow_diverged\""))
        XCTAssertGreaterThanOrEqual(
            archive.components(separatedBy: "\"code\":\"shadow_mismatch\"").count - 1, 2,
            "a divergent sample must record the summary and each finding"
        )
    }

    func testAFailureWithNoNextRowForThatMailboxDidNotContinueTheLane() throws {
        let adapter = adapterFor(try MessageStore.open(path: ":memory:"))
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        capture.noteFailed(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), error: RelayHTTPError(statusCode: 500, relayCode: nil, responseBody: "boom"))
        // A row for a *different* mailbox is not this mailbox continuing.
        capture.noteSucceeded(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlB, relayToken: Self.tokenB))
        XCTAssertFalse(capture.steps()[0].legacyContinuedLane)
    }

    func testAFailureThePassFollowedWithAnotherRowToTheSameMailboxDidContinueTheLane() throws {
        let adapter = adapterFor(try MessageStore.open(path: ":memory:"))
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        capture.noteFailed(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), error: RelayHTTPError(statusCode: 413, relayCode: nil, responseBody: "too big"))
        capture.noteSucceeded(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA))
        XCTAssertTrue(capture.steps()[0].legacyContinuedLane)
    }

    func testAComparisonWritesOnlyDiagnosticsNeverTheQueue() throws {
        let store = try MessageStore.open(path: ":memory:")
        let adapter = adapterFor(store)
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        capture.noteDeclined(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now)
        capture.noteSkippedRecipients([Self.recipient])
        capture.noteUnshadowed(2)
        adapter.finishPass(capture: capture, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: Self.now)

        XCTAssertEqual(try store.pendingRelayOutboundEnvelopes(limit: 64, nowMs: Self.now, skipRecipientUserIds: []).count, 0)
        XCTAssertEqual(try store.listContactRelayRejections().count, 0)
        XCTAssertEqual(try store.listContactRelayUnreachable().count, 0)
        XCTAssertEqual(try store.relayFetchCursor(configKey: "any").afterId, 0)
        XCTAssertTrue(try store.exportProtocolEventsJsonl().contains("\"rows_unshadowed\":2"))
    }

    func testACaptureIsBoundedAndSaysHowMuchItDropped() throws {
        let adapter = adapterFor(try MessageStore.open(path: ":memory:"))
        let capture = try XCTUnwrap(adapter.beginPass(nowMs: Self.now))
        let cap = Int(coreRelayShadowMaxRows())
        for _ in 0..<(cap + 5) {
            capture.noteSucceeded(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: Self.now, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA))
        }
        XCTAssertEqual(capture.steps().count, cap)
        XCTAssertEqual(capture.rowsDropped(), 5)
        // Dropped rows are unshadowed rows.
        XCTAssertEqual(capture.rowsUnshadowed(), 5)
    }

    // MARK: - helpers

    private var forbiddenTypeSubstrings: [String] {
        ["MessageStore", "URLSession", "URLRequest", "URLResponse", "URL", "Socket",
         "HTTPURLConnection", "RelayClient", "RelayActionDriver"]
    }

    private final class SamplerBox {
        var state = CoreRelayShadowSampler(dayIndex: 0, samplesToday: 0, lastSampleAtMs: 0)
    }

    /// Runs one pass that has a row worth comparing, and says whether it was
    /// sampled.
    private func sampledWithRow(_ adapter: RelayShadowAdapter, _ nowMs: Int64) -> Bool {
        guard let capture = adapter.beginPass(nowMs: nowMs) else { return false }
        capture.noteSucceeded(lane: .authored, msgId: msgId(), hopTtl: 4, recipientHint: hint(), recipientUserId: Self.recipient, sealedLen: Self.sealedLen, expiryMs: nowMs, endpoint: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA))
        let sampled = !capture.steps().isEmpty
        adapter.finishPass(capture: capture, own: RelayConfig(relayUrl: Self.urlA, relayToken: Self.tokenA), contacts: contacts(usable: true), nowMs: nowMs)
        return sampled
    }

    private func adapterFor(_ store: MessageStore, engine: RelayPassEngine = .legacy, shadowEnabled: Bool = true) -> RelayShadowAdapter {
        let box = SamplerBox()
        return RelayShadowAdapter(
            sink: RelayShadowReportSink { report, nowMs in store.noteRelayShadowReport(report: report, nowMs: nowMs) },
            passEngine: { engine },
            shadowEnabled: { shadowEnabled },
            loadSampler: { box.state },
            saveSampler: { box.state = $0 }
        )
    }

    private func contacts(usable: Bool) -> [CoreRelayShadowContact] {
        [CoreRelayShadowContact(userId: Self.recipient, relayUrl: nil, relayToken: nil, endpointUsable: usable)]
    }

    private func msgId() -> Data { Data(repeating: 0x11, count: 16) }
    private func hint() -> Data { Data(repeating: 0x22, count: 8) }
}
