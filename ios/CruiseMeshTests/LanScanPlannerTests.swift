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

    func testSweepThatMeetsAFriendItIsAlreadyLinkedToNeverArmsTheFullTier() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)

        // The sweep probed a friend an EARLIER sweep had already linked, so
        // it never reached a handshake at all -- and that still credits the
        // sweep (LanTransport.markSweepFoundFriend), because discovery
        // demonstrably works on this network.
        let generation = UUID()
        let foundPeer = lanSweepProbeFoundFriend(
            keyAlreadyAuthenticated: true,
            linkTableFull: false,
            authenticatedLinks: 1
        ) && lanSweepCreditApplies(
            sweepGeneration: generation,
            runningSweepGeneration: generation
        )
        planner.onScanCompleted(.local24, nowMs: 1_000, foundPeer: foundPeer)

        // A healthy network must not arm the expensive /20 tier.
        XCTAssertNil(planner.takeDueScan(nowMs: 1_000 + emptyDelay))
        XCTAssertNil(planner.takeDueScan(nowMs: 10 * 60 * minute))
    }

    /// The field failure's shape, at the planner: the approving phone had one
    /// endpoint it kept dialing and never reached, and it ran no sweep for 26
    /// minutes. Whatever else was wrong, the planner itself must never be the
    /// thing that wedges -- a peer that never answers, and a sweep that comes
    /// back empty enough to look like client isolation, must both leave the
    /// cheap /24 tier on its flat cadence indefinitely.
    func testAnEndpointThatNeverAnswersCannotStopTheLocalSweepCadence() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)

        // Worst case for the expensive tier: every probe timed out, so it is
        // pushed to its four-hour cap. The /24 tier is deliberately untouched.
        planner.onIsolationSuspected(nowMs: 0)

        var now: Int64 = 0
        for _ in 0..<12 {
            now += 5 * minute
            XCTAssertEqual(
                planner.takeDueScan(nowMs: now),
                .local24,
                "no sweep was due at \(now / minute) minutes into the network join"
            )
            // The sweep finds nobody, over and over, exactly as the field one did.
            planner.onScanCompleted(.local24, nowMs: now, foundPeer: false)
        }
    }

    func testPeerEvidenceStopsRewindingTheScheduleOnceItsPerNetworkBudgetIsSpent() {
        let budget = 3
        let planner = LanScanPlanner(
            localIntervalMs: Int64.max / 2,
            maxPeerEvidenceResets: budget
        )
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay), .fullSubnet)

        // Every advertisement carries an instance token its sender picks, so
        // "brand-new evidence" alone cannot bound this: a spray of fresh
        // tokens would otherwise reset the backoff and re-fire the /20 sweep
        // over and over on every phone in range.
        var now: Int64 = emptyDelay
        for _ in 0..<budget {
            now += 1_000
            XCTAssertTrue(planner.onPeerEvidence(nowMs: now))
            XCTAssertEqual(planner.takeDueScan(nowMs: now), .fullSubnet)
        }
        for _ in 0..<50 {
            now += 1_000
            XCTAssertFalse(planner.onPeerEvidence(nowMs: now))
        }
        // Back on the ordinary backoff ladder: nothing until the step elapses.
        XCTAssertNil(planner.takeDueScan(nowMs: now))
        XCTAssertNil(planner.takeDueScan(nowMs: now + 14 * minute))
    }

    func testEvidenceBudgetRefillsWhenTheNetworkIsRejoined() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2, maxPeerEvidenceResets: 1)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: emptyDelay), .fullSubnet)
        XCTAssertTrue(planner.onPeerEvidence(nowMs: emptyDelay + 1_000))
        XCTAssertFalse(planner.onPeerEvidence(nowMs: emptyDelay + 2_000))

        let rejoinAt = 26 * 60 * minute
        planner.onNetworkJoined(nowMs: rejoinAt)
        XCTAssertEqual(planner.takeDueScan(nowMs: rejoinAt), .local24)
        planner.onScanCompleted(.local24, nowMs: rejoinAt, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: rejoinAt + emptyDelay), .fullSubnet)
        XCTAssertTrue(planner.onPeerEvidence(nowMs: rejoinAt + emptyDelay + 1_000))
    }

    func testPeerEvidenceReportsNoScheduleChangeBeforeTheFullTierIsEligible() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)

        // Nothing armed yet, so there is nothing to hurry towards -- and the
        // budget stays intact for evidence that could actually matter.
        XCTAssertFalse(planner.onPeerEvidence(nowMs: 500))
        planner.onScanCompleted(.local24, nowMs: 1_000, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: 1_000 + emptyDelay), .fullSubnet)
        XCTAssertTrue(planner.onPeerEvidence(nowMs: 1_000 + emptyDelay + 1_000))
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

    func testIsolationDefersTheFullSweepToTheCapUntilPeerEvidenceResetsIt() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)

        let isolationAt: Int64 = 10_000
        planner.onIsolationSuspected(nowMs: isolationAt)
        XCTAssertNil(planner.takeDueScan(nowMs: isolationAt + 4 * 60 * minute - 1))
        XCTAssertEqual(planner.takeDueScan(nowMs: isolationAt + 4 * 60 * minute), .fullSubnet)

        let evidenceAt = isolationAt + 4 * 60 * minute + 1_000
        planner.onIsolationSuspected(nowMs: evidenceAt)
        planner.onPeerEvidence(nowMs: evidenceAt + 1_000)
        XCTAssertEqual(planner.takeDueScan(nowMs: evidenceAt + 1_000), .fullSubnet)
    }

    func testIsolationIsIgnoredWhileNoNetworkIsJoined() {
        let planner = LanScanPlanner(localIntervalMs: Int64.max / 2)
        planner.onNetworkJoined(nowMs: 0)
        XCTAssertEqual(planner.takeDueScan(nowMs: 0), .local24)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        planner.onNetworkLost()
        planner.onIsolationSuspected(nowMs: 1_000)

        // The deferral must not outlive the network it was measured on.
        planner.onNetworkJoined(nowMs: 2_000)
        XCTAssertEqual(planner.takeDueScan(nowMs: 2_000), .local24)
        planner.onScanCompleted(.local24, nowMs: 2_000, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: 2_000 + emptyDelay), .fullSubnet)
    }

    func testNetworkJoinResetsAnIsolationDeferral() {
        let planner = LanScanPlanner()
        planner.onNetworkJoined(nowMs: 0)
        _ = planner.takeDueScan(nowMs: 0)
        planner.onScanCompleted(.local24, nowMs: 0, foundPeer: false)
        planner.onIsolationSuspected(nowMs: 1_000)

        planner.onNetworkJoined(nowMs: 2_000)
        XCTAssertEqual(planner.takeDueScan(nowMs: 2_000), .local24)
        planner.onScanCompleted(.local24, nowMs: 2_000, foundPeer: false)
        XCTAssertEqual(planner.takeDueScan(nowMs: 2_000 + emptyDelay), .fullSubnet)
    }
}
