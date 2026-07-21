import XCTest
@testable import CruiseMesh

final class LanScanPlannerTests: XCTestCase {
    private let minute: Int64 = 60_000
    private let emptyDelay: Int64 = LanScanPlanner.emptyLocalSweepFullDelayMs

    func testNothingIsDueBeforeJoiningOrAfterLosingNetwork() {
        let planner = LanScanPlanner()
        XCTAssertNil(planner.takeDueScan(nowMs: 0))
        planner.onNetworkJoined(nowMs: 1_000)
        planner.onNetworkLost()
        XCTAssertNil(planner.takeDueScan(nowMs: 2_000))
    }

    func testJoinRunsLocalTierFirstAndFullSweepIsNotDueUntilAnEmptyLocalSweepArmsIt() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        // Not due at network-join anymore -- only after an empty /24 sweep.
        XCTAssertNil(planner.takeDueScan(nowMs: 1_000))
        planner.onScanCompleted(.local24, nowMs: 1_000, foundPeer: false)
        // Armed, but not immediately: a real delay applies.
        XCTAssertNil(planner.takeDueScan(nowMs: 1_000 + emptyDelay - 1))
        XCTAssertEqual(planner.takeDueScan(nowMs: 1_000 + emptyDelay), .fullSubnet)
    }

    func testLocalSweepThatFindsAPeerNeverArmsTheFullSweep() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 1_000, foundPeer: true)
        // A /24 sweep that found a peer must not arm the full tier at all.
        XCTAssertNil(planner.takeDueScan(nowMs: 1_000 + emptyDelay))
        XCTAssertNil(planner.takeDueScan(nowMs: 10 * 60 * minute))
    }

    func testOnceArmedALaterNonEmptyLocalSweepDoesNotDisarmOrRescheduleTheFullSweep() {
        let planner = LanScanPlanner(localIntervalMs: 5 * minute)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        // Armed at emptyDelay. A later local sweep (still before the full
        // sweep fires) that *does* find a peer must not push the already
        // -armed full-sweep schedule back out.
        XCTAssertEqual(planner.takeDueScan(nowMs: 5 * minute), .local24)
        planner.onScanCompleted(.local24, nowMs: 5 * minute, foundPeer: true)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay), .fullSubnet)
    }

    func testLocalTierKeepsFiveMinuteCadence() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay), .fullSubnet)
        XCTAssertNil(planner.takeDueScan(nowMs: 4 * minute))
        XCTAssertEqual(planner.takeDueScan(nowMs: 5 * minute), .local24)
        XCTAssertEqual(planner.takeDueScan(nowMs: 10 * minute), .local24)
    }

    func testFullSweepBacksOffFifteenMinutesThenOneHourThenFourHourCap() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)

        var now: Int64 = emptyDelay
        XCTAssertEqual(planner.takeDueScan(nowMs: now), .fullSubnet)
        for gap in [15 * minute, 60 * minute, 240 * minute, 240 * minute] {
            XCTAssertNil(planner.takeDueScan(nowMs: now + gap - 1))
            now += gap
            XCTAssertEqual(planner.takeDueScan(nowMs: now), .fullSubnet)
        }
    }

    func testPeerEvidenceIsANoOpBeforeTheFullSweepIsEligible() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        // No completed /24 sweep yet, so nothing is armed; evidence must not
        // conjure a full sweep out of nowhere.
        planner.onPeerEvidence(nowMs: 500)
        XCTAssertNil(planner.takeDueScan(nowMs: 500))
    }

    func testPeerEvidenceMakesFullSweepDueAndResetsBackoff() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay), .fullSubnet)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay + 15 * minute), .fullSubnet)

        let evidenceAt = emptyDelay + 2_000 + 15 * minute
        planner.onPeerEvidence(nowMs: evidenceAt)
        XCTAssertEqual(planner.takeDueScan(nowMs: evidenceAt), .fullSubnet)
        XCTAssertEqual(planner.takeDueScan(nowMs: evidenceAt + 15 * minute), .fullSubnet)
    }

    func testNetworkRejoinReanchorsLocalBeforeFullAndDisarmsTheFullTier() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay), .fullSubnet)

        let rejoinAt = 26 * 60 * minute
        planner.onNetworkJoined(nowMs: rejoinAt)
        XCTAssertEqual(planner.takeDueScan(nowMs: rejoinAt), .local24)
        // Disarmed on rejoin: not due even once the old empty-sweep delay
        // would have elapsed, and not due until a fresh /24 sweep completes.
        XCTAssertNil(planner.takeDueScan(nowMs: rejoinAt + emptyDelay))
        planner.onScanCompleted(.local24, nowMs: rejoinAt + emptyDelay, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: rejoinAt + emptyDelay + emptyDelay), .fullSubnet)
    }
}
