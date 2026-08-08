import XCTest
@testable import CruiseMesh

final class MeshStatusPillLogicTests: XCTestCase {
    private func suffix(
        _ health: RelayHealth,
        state: MeshRuntimeState = .meshing(nearby: 2),
        service: InternetDeliveryService? = .shorePass
    ) -> String? {
        MeshStatusPillLogic.faultSuffix(runtimeState: state, relayHealth: health, service: service)
    }

    // MARK: - The states that need a person

    func testExpiredPassIsNamed() {
        XCTAssertEqual(suffix(.expired(lastAttemptMs: 1)), "Shore Pass expired")
    }

    func testSuspendedPassIsNamed() {
        XCTAssertEqual(suffix(.suspended(lastAttemptMs: 1)), "Shore Pass suspended")
    }

    func testRejectedSetupIsNamedWithoutJargon() {
        XCTAssertEqual(suffix(.tokenRejected(lastAttemptMs: 1)), "Shore Pass setup was not accepted")
    }

    func testFullMailboxIsNamed() {
        XCTAssertEqual(suffix(.quotaFull(lastAttemptMs: 1)), "Shore Pass storage is full")
    }

    func testMessageTooLargeIsNamed() {
        XCTAssertEqual(suffix(.messageTooLarge(lastAttemptMs: 1)), "A message was too large to send")
    }

    func testACustomRelayIsNotCalledAShorePass() {
        XCTAssertEqual(
            suffix(.expired(lastAttemptMs: 1), service: .customRelay),
            "Internet delivery expired"
        )
    }

    // MARK: - The states that must stay quiet

    /// The whole point. Never buying a pass is the free default, and nearby
    /// delivery is working -- reporting it as a fault is what Android had to
    /// undo, and iOS must not acquire it while adding this.
    func testNeverConfiguredSaysNothing() {
        XCTAssertNil(suffix(.noConfig, service: nil))
    }

    /// Being at sea with no internet is the normal case this app exists for.
    func testNoInternetSaysNothing() {
        XCTAssertNil(suffix(.noInternet))
    }

    func testCheckingSaysNothing() {
        XCTAssertNil(suffix(.checking))
    }

    func testWorkingSaysNothing() {
        XCTAssertNil(suffix(.ok(lastSyncMs: 1)))
    }

    /// Transient and self-healing: `PassIndicator` calls these `.attention`,
    /// never worth acting on, so the pill stays out of it.
    func testTransientFaultsSayNothing() {
        XCTAssertNil(suffix(.failing(lastAttemptMs: 1)))
        XCTAssertNil(suffix(.rateLimited(lastAttemptMs: 1)))
    }

    /// A saved-but-unchecked pass reports `.noConfig` too. It must not be
    /// mistaken for a fault while the first check is still in flight.
    func testConfiguredButUncheckedSaysNothing() {
        XCTAssertNil(suffix(.noConfig))
    }

    // MARK: - Only while the mesh is up

    func testAStoppedMeshHasABiggerProblem() {
        XCTAssertNil(suffix(.expired(lastAttemptMs: 1), state: .stopped))
        XCTAssertNil(suffix(.expired(lastAttemptMs: 1), state: .starting))
    }

    func testQuietWithNoPeersNearbyToo() {
        XCTAssertEqual(
            suffix(.expired(lastAttemptMs: 1), state: .meshing(nearby: 0)),
            "Shore Pass expired"
        )
    }

    // MARK: - The gate agrees with Settings

    /// `PassIndicator.actionRequired` is the contract this logic keys off. If
    /// a health is ever reclassified there, this fails and whoever moved it
    /// has to decide what the pill says -- rather than the pill silently
    /// swallowing a fault.
    func testEveryActionRequiredHealthHasWording() {
        let actionRequired: [RelayHealth] = [
            .expired(lastAttemptMs: 1),
            .suspended(lastAttemptMs: 1),
            .tokenRejected(lastAttemptMs: 1),
            .quotaFull(lastAttemptMs: 1),
            .messageTooLarge(lastAttemptMs: 1),
        ]
        for health in actionRequired {
            XCTAssertEqual(PassIndicator.of(health, configured: true), .actionRequired, "\(health)")
            XCTAssertNotNil(suffix(health), "\(health) is action-required but the pill says nothing")
        }
    }

    // MARK: - The dot is the core's verdict

    private static let nowMs: Int64 = 1_760_000_000_000

    private func pill(
        _ health: RelayHealth = .ok(lastSyncMs: MeshStatusPillLogicTests.nowMs),
        state: MeshRuntimeState = .meshing(nearby: 0),
        nearby: Int = 0,
        bluetooth: BluetoothAvailability = .available,
        lanListening: Bool = true,
        service: InternetDeliveryService? = .shorePass,
        checkingSinceMs: Int64 = 0
    ) -> MeshStatusPillStatus {
        MeshStatusPillLogic.build(
            runtimeState: state,
            runtimeText: "Meshing",
            nearbyCount: nearby,
            bluetooth: bluetooth,
            lanListening: lanListening,
            relayHealth: health,
            service: service,
            checkingSinceMs: checkingSinceMs,
            nowMs: MeshStatusPillLogicTests.nowMs
        )
    }

    /// The divergence this closes. A friend in the room used to make the dot
    /// green whatever the pass was doing, over a Connection details page
    /// reading `Working, with limits` about the same phone at the same moment.
    func testAFriendInTheRoomNoLongerPaintsOverALapsedPass() {
        let status = pill(.expired(lastAttemptMs: 1), nearby: 2)
        XCTAssertEqual(status.health, CoreConnectionHealth.limited)
        XCTAssertEqual(status.dot, MeshStatusDotColor.amber)
        // And the words still name the fault beside the color.
        XCTAssertTrue(status.text.contains("Shore Pass expired"), status.text)
    }

    /// The three healthy dots are not severities: they say which path is
    /// carrying, and a person used to green meaning "someone is here" keeps
    /// that reading.
    func testAHealthyPhoneStillDistinguishesWhichPathIsCarrying() {
        XCTAssertEqual(pill(nearby: 1).dot, MeshStatusDotColor.green)
        XCTAssertEqual(pill().dot, MeshStatusDotColor.blue)
        XCTAssertEqual(pill(.noConfig, service: nil).dot, MeshStatusDotColor.neutral)
        XCTAssertEqual(pill(nearby: 1).health, CoreConnectionHealth.ready)
        XCTAssertEqual(pill().health, CoreConnectionHealth.ready)
        XCTAssertEqual(pill(.noConfig, service: nil).health, CoreConnectionHealth.ready)
    }

    /// Never buying a pass is the free default and nearby delivery works. A
    /// warning here would teach people to ignore the dot for when it finally
    /// means something.
    func testNoPassIsNotAWarning() {
        let status = pill(.noConfig, service: nil, checkingSinceMs: 0)
        XCTAssertEqual(status.health, CoreConnectionHealth.ready)
        XCTAssertEqual(status.dot, MeshStatusDotColor.neutral)
        XCTAssertEqual(status.text, "Meshing")
    }

    /// No verdict yet is not a warning either: the card shows a spinner there,
    /// and the pill has no room for one.
    func testAnUnresolvedCheckIsNeutralRatherThanColoured() {
        let status = pill(
            .checking,
            bluetooth: .starting,
            lanListening: false,
            checkingSinceMs: Self.nowMs
        )
        XCTAssertEqual(status.health, CoreConnectionHealth.checking)
        XCTAssertEqual(status.dot, MeshStatusDotColor.neutral)
    }

    /// A stopped mesh is the device's own problem, and the core says so.
    func testAStoppedMeshIsTheCoresNeedsAttention() {
        let status = pill(state: .stopped)
        XCTAssertEqual(status.health, CoreConnectionHealth.needsAttention)
        XCTAssertEqual(status.dot, MeshStatusDotColor.amber)
    }

    /// The pill and the Connection details health card consume one
    /// classification, so they cannot report different things about one phone.
    func testThePillAndTheHealthCardReachTheSameVerdict() {
        // No friends nearby in either input: the page counts friends with a
        // live link and the pill counts peers, and this is about the verdict,
        // not about reconciling two counts. The core's evidence carries the
        // count; its classification does not consult it.
        let cases: [(RelayHealth, InternetDeliveryService?, MeshRuntimeState)] = [
            (.ok(lastSyncMs: Self.nowMs), .shorePass, .meshing(nearby: 0)),
            (.expired(lastAttemptMs: 1), .shorePass, .meshing(nearby: 0)),
            (.noInternet, .shorePass, .meshing(nearby: 0)),
            (.noConfig, nil, .meshing(nearby: 0)),
            (.ok(lastSyncMs: Self.nowMs), .shorePass, .stopped),
        ]
        for (health, service, runtime) in cases {
            let status = pill(health, state: runtime, service: service)
            let page = ConnectionDetailsLogic.buildState(
                runtimeState: runtime,
                bluetoothAvailability: .available,
                directPaths: [:],
                relayHealth: health,
                relayConfigured: service != nil,
                lanListening: true,
                bluetoothAudioActive: false,
                presenceLastSeen: [:],
                contactLastSeen: [:],
                snapshot: ConnectionStoreSnapshot(people: [], activity: [], loadedAtMs: Self.nowMs),
                checkingSinceMs: 0,
                refreshing: false,
                nowMs: Self.nowMs
            )
            XCTAssertEqual(status.health, page.health.state, "\(health) \(runtime)")
        }
    }
}
