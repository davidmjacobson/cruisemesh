import XCTest
@testable import CruiseMesh

final class MeshStatusPillLogicTests: XCTestCase {
    private func suffix(
        _ health: RelayHealth,
        state: MeshRuntimeState = .meshing(nearby: 2),
        service: InternetDeliveryService? = .cruisePass
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

    func testACustomRelayIsNotCalledACruisePass() {
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
}
