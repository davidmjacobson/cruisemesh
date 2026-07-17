import XCTest
@testable import CruiseMesh

/// Mirrors Android's RelayPushBackoffTest.kt case-for-case.
final class RelayPushBackoffTests: XCTestCase {
    func testFreshBackoffStartsAtInitialDelay() {
        let backoff = RelayPushBackoff(initialBackoffMs: 2_000)
        XCTAssertEqual(backoff.nextDelayMs(), 2_000)
    }

    func testDelayDoublesOnEachConsecutiveFailureUpToCap() {
        let backoff = RelayPushBackoff(initialBackoffMs: 1_000, maxBackoffMs: 5_000)

        backoff.recordFailure()
        XCTAssertEqual(backoff.nextDelayMs(), 2_000)

        backoff.recordFailure()
        XCTAssertEqual(backoff.nextDelayMs(), 4_000)

        backoff.recordFailure() // would be 8_000, capped at 5_000
        XCTAssertEqual(backoff.nextDelayMs(), 5_000)

        backoff.recordFailure() // stays capped
        XCTAssertEqual(backoff.nextDelayMs(), 5_000)
    }

    func testRecordSuccessResetsDelayBackToInitialValue() {
        let backoff = RelayPushBackoff(initialBackoffMs: 1_000, maxBackoffMs: 60_000)
        for _ in 0..<5 { backoff.recordFailure() }
        XCTAssertEqual(backoff.nextDelayMs(), 32_000)

        backoff.recordSuccess()

        XCTAssertEqual(backoff.nextDelayMs(), 1_000)
    }

    func testNeverOverflowsEvenAfterManyConsecutiveFailures() {
        let backoff = RelayPushBackoff(initialBackoffMs: 1_000, maxBackoffMs: 60_000)
        for _ in 0..<1_000 { backoff.recordFailure() }
        XCTAssertEqual(backoff.nextDelayMs(), 60_000)
    }

    func testNeverFailedBackoffReportsZeroConsecutiveFailuresWorthOfDelayGrowth() {
        // Sanity check that a brand new instance and a recordSuccess()-reset
        // instance are indistinguishable -- both start the reconnect loop at
        // the floor, never at some remembered "give up" state.
        let fresh = RelayPushBackoff(initialBackoffMs: 3_000, maxBackoffMs: 30_000)
        let reset = RelayPushBackoff(initialBackoffMs: 3_000, maxBackoffMs: 30_000)
        for _ in 0..<3 { reset.recordFailure() }
        reset.recordSuccess()

        XCTAssertEqual(fresh.nextDelayMs(), reset.nextDelayMs())
    }
}
