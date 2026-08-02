import Network
import XCTest
@testable import CruiseMesh

final class LanSweepOutcomesTests: XCTestCase {
    func testSummaryCountsEveryOutcomeAndRendersOneLogLine() {
        var summary = LanSweepOutcomeSummary()
        for outcome in [
            LanSweepProbeOutcome.connected,
            .refused,
            .timedOut,
            .denied,
            .other,
        ] {
            summary.record(outcome)
        }

        XCTAssertEqual(summary.probed, 5)
        XCTAssertEqual(
            summary.logLine(prefixLength: 16),
            "Sweep complete (/16): 5 probed, 1 connected, 1 refused, 1 timed out, 1 denied, 1 other."
        )
    }

    func testProbeFailuresAreClassified() {
        XCTAssertEqual(classifyLanSweepProbeFailure(.posix(.ECONNREFUSED)), .refused)
        XCTAssertEqual(classifyLanSweepProbeFailure(.posix(.ETIMEDOUT)), .timedOut)
        XCTAssertEqual(classifyLanSweepProbeFailure(.posix(.EPERM)), .denied)
        XCTAssertEqual(classifyLanSweepProbeFailure(.posix(.EACCES)), .denied)
        // kDNSServiceErr_PolicyDenied
        XCTAssertEqual(classifyLanSweepProbeFailure(.dns(-65570)), .denied)
        XCTAssertEqual(classifyLanSweepProbeFailure(.posix(.EHOSTUNREACH)), .other)
        XCTAssertEqual(classifyLanSweepProbeFailure(nil), .other)
    }

    func testAllSilentBroadSweepIndicatesIsolation() {
        XCTAssertEqual(lanSweepVerdict(summary(timedOut: 253)), .isolationSuspected)
    }

    func testDeniedSweepIndicatesPolicyBlockWithoutIsolation() {
        XCTAssertEqual(
            lanSweepVerdict(summary(timedOut: 252, denied: 1)),
            .blockedByPolicy
        )
    }

    func testRefusedHostMakesAnEmptySweepHealthyRatherThanIsolated() {
        XCTAssertEqual(
            lanSweepVerdict(summary(refused: 1, timedOut: 252)),
            .healthyButEmpty
        )
    }

    func testAConnectedProbeMeansTheSweepFoundAPeer() {
        XCTAssertEqual(
            lanSweepVerdict(summary(connected: 1, timedOut: 252)),
            .foundPeer
        )
    }

    func testATooNarrowSilentSweepIsInconclusive() {
        XCTAssertEqual(lanSweepVerdict(summary(timedOut: 252)), .inconclusive)
    }

    func testIsolationAppearsOnlyAfterACompletedBroadSweep() {
        let tracker = LanSweepDisplayTracker()

        XCTAssertEqual(tracker.current(), .none)
        XCTAssertEqual(tracker.onNetworkJoined(), .checking)
        XCTAssertEqual(tracker.onSweepStarted(), .checking)
        XCTAssertEqual(tracker.onSweepCompleted(summary(timedOut: 252)), .none)

        tracker.onSweepStarted()
        XCTAssertEqual(tracker.onSweepCompleted(summary(timedOut: 253)), .isolationSuspected)
    }

    func testPolicyDenialTakesPrecedenceOverIsolationInTheDisplay() {
        let tracker = LanSweepDisplayTracker()
        tracker.onNetworkJoined()
        tracker.onSweepStarted()

        XCTAssertEqual(
            tracker.onSweepCompleted(summary(timedOut: 252, denied: 1)),
            .blockedByPolicy
        )
    }

    func testNetworkChangeReplacesAStaleVerdictWithChecking() {
        let tracker = LanSweepDisplayTracker()
        tracker.onSweepCompleted(summary(timedOut: 253))

        XCTAssertEqual(tracker.onNetworkJoined(), .checking)
    }

    func testPeerEvidenceClearsACompletedIsolationVerdict() {
        let tracker = LanSweepDisplayTracker()
        tracker.onSweepCompleted(summary(timedOut: 253))

        XCTAssertEqual(tracker.onPeerEvidence(), .none)
    }

    func testLateSweepCompletionCannotResurrectVerdictAfterPeerEvidence() {
        let tracker = LanSweepDisplayTracker()
        tracker.onNetworkJoined()
        tracker.onSweepStarted()
        tracker.onPeerEvidence()

        XCTAssertEqual(tracker.onSweepCompleted(summary(timedOut: 253)), .none)
    }

    func testLosingWifiClearsEverySweepDisplayState() {
        let tracker = LanSweepDisplayTracker()
        tracker.onNetworkJoined()

        XCTAssertEqual(tracker.onNetworkLost(), .none)
    }

    private func summary(
        connected: Int = 0,
        refused: Int = 0,
        timedOut: Int = 0,
        denied: Int = 0,
        other: Int = 0
    ) -> LanSweepOutcomeSummary {
        LanSweepOutcomeSummary(
            connected: connected,
            refused: refused,
            timedOut: timedOut,
            denied: denied,
            other: other
        )
    }
}
