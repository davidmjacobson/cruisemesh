import XCTest
@testable import CruiseMesh

/// The protocol-event ring across the UniFFI boundary.
///
/// Deliberately not a second copy of the ring's rules -- Rust owns eviction,
/// redaction, the schema and the pseudonyms, and `core/tests` is where those
/// are proved. What this pins is the boundary itself, which Rust tests cannot
/// reach: that the export arrives as one whole string rather than a truncated
/// one, that the gating call returns a real boolean, and that handing the
/// spray policy a `MessageStore` -- the first place this project passes one
/// core object into another across the FFI -- actually connects the two.
///
/// The Android twin is `ProtocolEventBindingTest.kt`, and it makes the same
/// three assertions in the same order.
final class ProtocolEventBindingTests: XCTestCase {
    private let mailboxKey = "https://relay.example.invalid/|cmdep1-secrettoken"
    private let peerKey = "aabbccddeeff"
    private let linkKey = "link-1"
    private let now: Int64 = 1_700_000_000_000

    private func makeStore() throws -> MessageStore {
        try MessageStore.open(path: ":memory:")
    }

    func testExportedRingCrossesTheBoundaryAsOneWholeDocument() throws {
        let store = try makeStore()
        try store.clearProtocolEvents()
        XCTAssertFalse(try store.hasProtocolEvents(), "a cleared ring has nothing to send")

        // A relay page that moved the frontier: a real emit point, driven the
        // way the mailbox walk drives it.
        _ = try store.advanceRelayFetchCursor(
            configKey: mailboxKey,
            pageNextCursor: 100,
            pageFullyProcessed: true
        )
        XCTAssertTrue(try store.hasProtocolEvents())

        let jsonl = try store.exportProtocolEventsJsonl()
        let lines = jsonl.trimmingCharacters(in: .whitespacesAndNewlines)
            .split(separator: "\n", omittingEmptySubsequences: true)
        XCTAssertGreaterThanOrEqual(lines.count, 2, "expected a header and a record: \(jsonl)")
        XCTAssertTrue(lines[0].contains("\"record\":\"header\""))
        XCTAssertTrue(lines[0].contains("cruisemesh.protocol-event/v1"))
        XCTAssertTrue(lines.contains { $0.contains("\"code\":\"frontier_advanced\"") })
        XCTAssertTrue(jsonl.hasSuffix("\n"), "the archive ends with a newline")

        // The token in the config key is exactly what must not survive the trip.
        XCTAssertFalse(jsonl.contains("cmdep1-"))
        XCTAssertFalse(jsonl.contains("://"))
    }

    func testAttachingTheStoreCarriesSprayDecisionsIntoTheRing() throws {
        let store = try makeStore()
        try store.clearProtocolEvents()
        let policy = CoreSprayPolicy()
        policy.attachEventJournal(store: store)

        let plan = CoreSprayPlanShape(
            carried: CoreSprayLanePlan(setDigest: 11, bytes: 4096),
            ownOutbound: CoreSprayLanePlan(setDigest: 22, bytes: 512),
            ownReceipts: CoreSprayLanePlan(setDigest: 0, bytes: 0)
        )
        _ = policy.admitPlan(peerKey: peerKey, linkKey: linkKey, lanes: plan, nowMs: now)
        // The same advertised set again inside the re-offer interval.
        _ = policy.admitPlan(peerKey: peerKey, linkKey: linkKey, lanes: plan, nowMs: now + 1000)

        let jsonl = try store.exportProtocolEventsJsonl()
        XCTAssertTrue(jsonl.contains("spray_admitted"))
        XCTAssertTrue(jsonl.contains("spray_suppressed"))
        XCTAssertFalse(jsonl.contains(peerKey), "the raw peer key must not reach the archive")
    }

    func testClearingTheRingIsWhatDeleteCapturedDiagnosticsNeedsItToBe() throws {
        let store = try makeStore()
        _ = try store.advanceRelayFetchCursor(
            configKey: mailboxKey,
            pageNextCursor: 7,
            pageFullyProcessed: true
        )
        XCTAssertTrue(try store.hasProtocolEvents())
        try store.clearProtocolEvents()
        XCTAssertFalse(try store.hasProtocolEvents())

        let remaining = try store.exportProtocolEventsJsonl()
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(separator: "\n", omittingEmptySubsequences: true)
        XCTAssertEqual(remaining.count, 1, "a cleared ring exports a header and nothing else")
    }
}
