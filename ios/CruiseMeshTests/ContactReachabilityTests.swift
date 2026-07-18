import XCTest
@testable import CruiseMesh

final class ContactReachabilityTests: XCTestCase {
    func testDirectLinkWins() {
        XCTAssertEqual(
            ContactReachability.compute(
                directLink: true,
                presenceLastSeenMs: nil,
                selfRelayHealthy: false,
                peerLastSeenMs: nil,
                nearbyPeerCount: 0,
                nowMs: 0
            ),
            .nearby
        )
    }

    func testFreshPresenceNeedsHealthyRelay() {
        XCTAssertEqual(
            ContactReachability.compute(
                directLink: false,
                presenceLastSeenMs: 0,
                selfRelayHealthy: true,
                peerLastSeenMs: 0,
                nearbyPeerCount: 0,
                nowMs: ContactReachability.presenceOnlineWindowMs
            ),
            .onlineRelay
        )
        XCTAssertEqual(
            ContactReachability.compute(
                directLink: false,
                presenceLastSeenMs: 0,
                selfRelayHealthy: false,
                peerLastSeenMs: 0,
                nearbyPeerCount: 0,
                nowMs: ContactReachability.presenceOnlineWindowMs
            ),
            .recent
        )
    }

    func testRecentBoundaryAndMeshCarryFallback() {
        XCTAssertEqual(
            ContactReachability.compute(
                directLink: false,
                presenceLastSeenMs: nil,
                selfRelayHealthy: false,
                peerLastSeenMs: 0,
                nearbyPeerCount: 1,
                nowMs: ContactReachability.recentWindowMs
            ),
            .recent
        )
        XCTAssertEqual(
            ContactReachability.compute(
                directLink: false,
                presenceLastSeenMs: nil,
                selfRelayHealthy: false,
                peerLastSeenMs: 0,
                nearbyPeerCount: 1,
                nowMs: ContactReachability.recentWindowMs + 1
            ),
            .meshCarry
        )
    }

    func testOfflineAndCopy() {
        XCTAssertEqual(
            ContactReachability.compute(
                directLink: false,
                presenceLastSeenMs: nil,
                selfRelayHealthy: false,
                peerLastSeenMs: nil,
                nearbyPeerCount: 0,
                nowMs: 1
            ),
            .offline
        )
        XCTAssertEqual(
            ContactReachability.chatHeaderCopy(.nearby, peerLastSeenMs: nil, nowMs: 0),
            "Nearby via Bluetooth"
        )
        XCTAssertEqual(
            ContactReachability.chatHeaderCopy(.recent, peerLastSeenMs: 0, nowMs: 5 * 60_000),
            "Active 5m ago"
        )
        XCTAssertNil(ContactReachability.contentDescriptionSuffix(.offline))
    }
}
