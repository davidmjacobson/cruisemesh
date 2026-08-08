import XCTest
@testable import CruiseMesh

/// The iOS half of issue #280: that this shell asks core rather than deciding,
/// and that the answers arrive intact across the FFI boundary.
///
/// The decisions themselves are pinned in `core/src/spray_policy.rs`, with
/// table-driven cases and mutation-verified assertions. What is worth testing
/// here is the wiring: that the budgets iOS now sprays with are core's numbers
/// (the three `MeshDefaults` constants this branch deleted), that a peer key is
/// a peer and a link key is a link, and that the enum cases survive the
/// crossing. Mirrors `SprayPolicyTest.kt` on Android.
///
/// Every call passes an explicit monotonic `nowMs`, as the production call
/// sites do through `SprayPolicy.nowMs`.
final class SprayPolicyTests: XCTestCase {

    private let peer = Data(repeating: 0x11, count: 16)
    private let otherPeer = Data(repeating: 0x22, count: 16)
    private let link = "AA:BB:CC:DD:EE:01"
    private let otherLink = "AA:BB:CC:DD:EE:02"

    override func setUp() {
        super.setUp()
        SprayPolicy.reset()
    }

    func testPerEncounterByteBudgetsNowComeFromCore() {
        // These three numbers were `MeshDefaults` constants, duplicated in
        // Android's InboundEnvelopeProcessor.kt. This test is what makes their
        // deletion permanent: if someone reintroduces a shell constant, the
        // shell and core answers can differ again, and the only place that can
        // now supply them is here.
        let gate = SprayPolicy.maySpray(
            peerUserId: peer,
            address: link,
            trigger: .firstContact,
            nowMs: 0
        )
        XCTAssertTrue(gate.allow)
        XCTAssertEqual(gate.carriedBudgetBytes, 256 * 1024)
        XCTAssertEqual(gate.ownOutboundBudgetBytes, 256 * 1024)
        XCTAssertEqual(gate.ownReceiptBudgetBytes, 64 * 1024)
    }

    func testFreshEncounterSyncsAtOnceAndReconnectChurnDoesNot() {
        let first = SprayPolicy.maySpray(
            peerUserId: peer,
            address: link,
            trigger: .firstContact,
            nowMs: 0
        )
        XCTAssertTrue(first.allow, "two phones meeting must never be gated")
        XCTAssertEqual(first.reason, .firstContact)
        SprayPolicy.noteDigestSent(peerUserId: peer, address: link, nowMs: 0)

        for nowMs in [Int64(200), 1_000, 30_000] {
            let churn = SprayPolicy.maySpray(
                peerUserId: peer,
                address: link,
                trigger: .reconnect,
                nowMs: nowMs
            )
            XCTAssertFalse(churn.allow, "reconnect at \(nowMs)ms")
            XCTAssertGreaterThan(churn.retryAfterMs, 0, "a denial must name its expiry")
        }
        XCTAssertFalse(
            SprayPolicy.maySpray(
                peerUserId: peer,
                address: otherLink,
                trigger: .reconnect,
                nowMs: 500
            ).allow,
            "a new address is not a new peer"
        )
        XCTAssertTrue(
            SprayPolicy.maySpray(
                peerUserId: otherPeer,
                address: link,
                trigger: .firstContact,
                nowMs: 500
            ).allow,
            "a different phone is unaffected"
        )
    }

    func testAnsweringTheDigestOurOwnSprayProvokedIsNotChurn() {
        XCTAssertTrue(
            SprayPolicy.maySpray(peerUserId: peer, address: link, trigger: .firstContact, nowMs: 0).allow
        )
        SprayPolicy.noteDigestSent(peerUserId: peer, address: link, nowMs: 0)
        let answer = SprayPolicy.maySpray(
            peerUserId: peer,
            address: link,
            trigger: .peerDigest,
            nowMs: 400
        )
        XCTAssertTrue(answer.allow, "the peer's half of one exchange")
        XCTAssertEqual(answer.reason, .exchangeOpen)
    }

    /// A one-lane plan (own outbound), for cases that are not about lanes.
    private func ownLane(_ setDigest: UInt64, _ bytes: UInt64) -> CoreSprayPlanShape {
        CoreSprayPlanShape(
            carried: Self.emptyLane,
            ownOutbound: CoreSprayLanePlan(setDigest: setDigest, bytes: bytes),
            ownReceipts: Self.emptyLane
        )
    }

    private static let emptyLane = CoreSprayLanePlan(setDigest: 0, bytes: 0)

    func testAnUnchangedAdvertisedSetIsNotRespayedAtFullSize() {
        let sent = SprayPolicy.admitPlan(
            peerUserId: peer,
            address: link,
            lanes: ownLane(0xABCD, 8_192),
            nowMs: 0
        )
        XCTAssertTrue(sent.send)
        XCTAssertEqual(sent.reason, .setChanged)

        let repeated = SprayPolicy.admitPlan(
            peerUserId: peer,
            address: link,
            lanes: ownLane(0xABCD, 8_192),
            nowMs: 60_000
        )
        XCTAssertFalse(repeated.send, "the 28 identical sprays")
        XCTAssertEqual(repeated.reason, .identicalSuppressed)
        XCTAssertGreaterThan(repeated.reofferInMs, 0, "suppression must expire")

        let changed = SprayPolicy.admitPlan(
            peerUserId: peer,
            address: link,
            lanes: ownLane(0x1234, 8_192),
            nowMs: 60_001
        )
        XCTAssertTrue(changed.send, "a set change sprays immediately")
    }

    func testAnInvariantAuthoredLaneGoesQuietWhileTheCarriedWalkProceeds() {
        // The recorded shape: authored invariant at 16 envelopes across every
        // spray while the carried lane walked its cursor. `sprayDigestPlanTo`
        // sends exactly the lanes this admission names, so the per-lane answer
        // has to survive the crossing.
        var carriedSends = 0
        var authoredSends = 0
        for round in 0..<6 {
            let admission = SprayPolicy.admitPlan(
                peerUserId: peer,
                address: link,
                lanes: CoreSprayPlanShape(
                    carried: CoreSprayLanePlan(setDigest: UInt64(round) + 1, bytes: 8_192),
                    ownOutbound: CoreSprayLanePlan(setDigest: 0xA17, bytes: 16_384),
                    ownReceipts: Self.emptyLane
                ),
                nowMs: Int64(round) * 200
            )
            if admission.sendCarried { carriedSends += 1 }
            if admission.sendOwnOutbound { authoredSends += 1 }
        }
        XCTAssertEqual(carriedSends, 6, "the carried walk must not be suppressed")
        XCTAssertEqual(authoredSends, 1, "an invariant authored set is offered once")
    }

    func testADisconnectResetsNeitherThePeerCadenceNorTheLinkAllowance() {
        XCTAssertTrue(
            SprayPolicy.maySpray(peerUserId: peer, address: link, trigger: .firstContact, nowMs: 0).allow
        )
        SprayPolicy.noteDigestSent(peerUserId: peer, address: link, nowMs: 0)
        // Everything this encounter queued outside the plan -- the receipt
        // repair pass, the per-missing-message re-send loop, the group catch-up
        // and the carry drain -- is charged here, exactly as `respondToDigest`
        // and `handleHello` do.
        SprayPolicy.noteBytesQueued(address: link, bytes: 576 * 1024, nowMs: 0)
        // This is what MeshController.recordPeerDisconnected does. It resets
        // nothing: a disconnect is what reconnect churn produces.
        SprayPolicy.noteLinkClosed(address: link, nowMs: 0)
        let gate = SprayPolicy.maySpray(peerUserId: peer, address: link, trigger: .peerDigest, nowMs: 1)
        XCTAssertFalse(gate.allow)
        XCTAssertEqual(gate.reason, .linkBurstExhausted)
        XCTAssertGreaterThan(gate.retryAfterMs, 0, "a denial must name its expiry")
    }

    func testProgressEvidenceClearsTheReceiptQuietBackoff() {
        XCTAssertTrue(
            SprayPolicy.maySpray(peerUserId: peer, address: link, trigger: .firstContact, nowMs: 0).allow
        )
        var now: Int64 = 0
        for round in 0..<4 {
            _ = SprayPolicy.admitPlan(
                peerUserId: peer,
                address: link,
                lanes: ownLane(UInt64(round), 1_024),
                nowMs: now
            )
            now += 61_000
        }
        let stretched = SprayPolicy.maySpray(
            peerUserId: peer,
            address: link,
            trigger: .reconnect,
            nowMs: now
        )
        XCTAssertFalse(stretched.allow)
        XCTAssertEqual(stretched.reason, .receiptQuietBackoff)

        // A receipt (or a confirmed carried delivery) is the evidence that
        // clears it — the two places MeshController reports.
        SprayPolicy.noteReceiptProgress(peerUserId: peer, nowMs: now)
        XCTAssertTrue(
            SprayPolicy.maySpray(peerUserId: peer, address: link, trigger: .reconnect, nowMs: now).allow
        )
    }

    func testTheReArmHorizonIsCoresNumber() {
        // MeshController.rearmGatedSpray consults gate.retryWorthArming rather
        // than comparing against a local constant; this is the number behind
        // that flag, and it lives in core.
        XCTAssertEqual(SprayPolicy.retryArmMaxMs, 60_000)
    }
}
