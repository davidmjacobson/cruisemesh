import XCTest
@testable import CruiseMesh

final class ReconnectBackoffTrackerTests: XCTestCase {
    func testNeverSeenAddressIsImmediatelyEligible() {
        let tracker = ReconnectBackoffTracker()
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 0))
    }

    func testFailureBlocksRetryUntilBackoffElapses() {
        let tracker = ReconnectBackoffTracker(initialBackoffMs: 1_000)
        tracker.recordFailure(address: "AA:BB", nowMs: 0)
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 500))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 1_000))
    }

    func testBackoffDoublesOnEachFailureUpToCap() {
        let tracker = ReconnectBackoffTracker(
            initialBackoffMs: 1_000,
            maxBackoffMs: 5_000,
            maxConsecutiveFailures: 100
        )
        tracker.recordFailure(address: "AA:BB", nowMs: 0) // next eligible at 1_000
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 999))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 1_000))

        tracker.recordFailure(address: "AA:BB", nowMs: 1_000) // +2_000 → 3_000
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 2_999))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 3_000))

        tracker.recordFailure(address: "AA:BB", nowMs: 3_000) // +4_000 → 7_000
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 6_999))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 7_000))

        tracker.recordFailure(address: "AA:BB", nowMs: 7_000) // capped at 5_000 → 12_000
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 11_999))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 12_000))
    }

    func testAddressPastFailureBudgetDecaysToSlowProbing() {
        let tracker = ReconnectBackoffTracker(
            initialBackoffMs: 1,
            maxBackoffMs: 1,
            maxConsecutiveFailures: 3,
            giveUpProbeMs: 1_000
        )
        XCTAssertFalse(tracker.isGivenUp(address: "AA:BB"))
        for _ in 0..<3 {
            tracker.recordFailure(address: "AA:BB", nowMs: 2)
        }
        XCTAssertTrue(tracker.isGivenUp(address: "AA:BB"))
        // Refused only until the probe interval elapses — a permanent refusal
        // wedged live links whose current address ate transient failures
        // during a Wi-Fi teardown (observed on-device 2026-07-24; only a
        // Bluetooth cycle recovered it).
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 1_001))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 1_002))
        // A failed probe re-arms the probe interval, still not a refusal.
        tracker.recordFailure(address: "AA:BB", nowMs: 1_002)
        XCTAssertTrue(tracker.isGivenUp(address: "AA:BB"))
        XCTAssertFalse(tracker.canAttempt(address: "AA:BB", nowMs: 2_001))
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 2_002))
        XCTAssertEqual(tracker.retryDelayMs(address: "AA:BB", nowMs: 1_002), 1_000)
    }

    func testRecordSuccessClearsFailureHistory() {
        let tracker = ReconnectBackoffTracker(
            initialBackoffMs: 1_000,
            maxConsecutiveFailures: 2
        )
        tracker.recordFailure(address: "AA:BB", nowMs: 0)
        tracker.recordFailure(address: "AA:BB", nowMs: 0)
        XCTAssertTrue(tracker.isGivenUp(address: "AA:BB"))

        tracker.recordSuccess(address: "AA:BB")

        XCTAssertFalse(tracker.isGivenUp(address: "AA:BB"))
        XCTAssertEqual(tracker.failureCount(address: "AA:BB"), 0)
        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 0))
    }

    func testGivingUpOnOneAddressNeverBlocksAnother() {
        let tracker = ReconnectBackoffTracker(
            initialBackoffMs: 1_000,
            maxConsecutiveFailures: 2
        )
        tracker.recordFailure(address: "STALE", nowMs: 0)
        tracker.recordFailure(address: "STALE", nowMs: 0)
        XCTAssertTrue(tracker.isGivenUp(address: "STALE"))

        XCTAssertTrue(tracker.canAttempt(address: "FRESH", nowMs: 0))
        XCTAssertFalse(tracker.isGivenUp(address: "FRESH"))
    }

    func testRetryDelayTracksNextEligibleAttempt() {
        let tracker = ReconnectBackoffTracker(initialBackoffMs: 1_000, maxBackoffMs: 10_000)
        tracker.recordFailure(address: "AA:BB", nowMs: 500)

        XCTAssertEqual(tracker.retryDelayMs(address: "AA:BB", nowMs: 750), 750)
        XCTAssertEqual(tracker.retryDelayMs(address: "AA:BB", nowMs: 1_500), 0)
    }

    func testClearForgetsAllAddresses() {
        let tracker = ReconnectBackoffTracker(initialBackoffMs: 1_000, maxBackoffMs: 10_000)
        tracker.recordFailure(address: "AA:BB", nowMs: 0)
        tracker.clear()

        XCTAssertTrue(tracker.canAttempt(address: "AA:BB", nowMs: 0))
        XCTAssertNil(tracker.retryDelayMs(address: "AA:BB", nowMs: 0))
    }
}
