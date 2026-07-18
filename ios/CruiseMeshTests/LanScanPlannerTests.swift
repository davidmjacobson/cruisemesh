import XCTest
@testable import CruiseMesh

final class LanScanPlannerTests: XCTestCase {
    private let minute: Int64 = 60_000

    func testNothingIsDueBeforeJoiningOrAfterLosingNetwork() {
        let planner = LanScanPlanner()
        XCTAssertNil(planner.takeDueScan(nowMs: 0))
        planner.onNetworkJoined(nowMs: 1_000)
        planner.onNetworkLost()
        XCTAssertNil(planner.takeDueScan(nowMs: 2_000))
    }

    func testJoinRunsLocalTierFirstAndEscalatesOnlyAfterCompletion() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        XCTAssertNil(planner.takeDueScan(nowMs: 1_000))
        planner.onScanCompleted(.local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: 2_000), .fullSubnet)
    }

    func testLocalTierKeepsFiveMinuteCadence() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: 1_000), .fullSubnet)
        XCTAssertNil(planner.takeDueScan(nowMs: 4 * minute))
        XCTAssertEqual(planner.takeDueScan(nowMs: 5 * minute), .local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: 10 * minute), .local24)
    }

    func testFullSweepBacksOffFifteenMinutesThenOneHourThenFourHourCap() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24)

        var now: Int64 = 1_000
        XCTAssertEqual(planner.takeDueScan(nowMs: now), .fullSubnet)
        for gap in [15 * minute, 60 * minute, 240 * minute, 240 * minute] {
            XCTAssertNil(planner.takeDueScan(nowMs: now + gap - 1))
            now += gap
            XCTAssertEqual(planner.takeDueScan(nowMs: now), .fullSubnet)
        }
    }

    func testPeerEvidenceMakesFullSweepDueAndResetsBackoff() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: 1_000), .fullSubnet)
        XCTAssertEqual(planner.takeDueScan(nowMs: 1_000 + 15 * minute), .fullSubnet)

        let evidenceAt = 3_000 + 15 * minute
        planner.onPeerEvidence(nowMs: evidenceAt)
        XCTAssertEqual(planner.takeDueScan(nowMs: evidenceAt), .fullSubnet)
        XCTAssertEqual(planner.takeDueScan(nowMs: evidenceAt + 15 * minute), .fullSubnet)
    }

    func testNetworkRejoinReanchorsLocalBeforeFull() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: 1_000), .fullSubnet)

        let rejoinAt = 26 * 60 * minute
        planner.onNetworkJoined(nowMs: rejoinAt)
        XCTAssertEqual(planner.takeDueScan(nowMs: rejoinAt), .local24)
        XCTAssertNil(planner.takeDueScan(nowMs: rejoinAt + 1_000))
        planner.onScanCompleted(.local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: rejoinAt + 2_000), .fullSubnet)
    }
}
