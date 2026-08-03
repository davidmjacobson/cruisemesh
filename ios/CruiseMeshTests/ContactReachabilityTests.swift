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

    // -- selfRelayHealthy(pushHealthy:) (battery, 2026-07-21) ---------------

    func testHealthyPushSocketKeepsOwnRelayHealthCurrentPastTwoPollStalenessWindow() {
        // The relay poll backs off to a 900s safety net while push is
        // healthy and foregrounded, so a live push socket -- not just a
        // recent poll -- must be able to keep this current. 15+ minutes of
        // quiet (well past the old 2*60s=120s staleness window) must not
        // degrade the reading while pushHealthy is true.
        let fifteenMinutesOfQuiet: Int64 = 15 * 60_000
        XCTAssertTrue(
            ContactReachability.selfRelayHealthy(
                .ok(lastSyncMs: 0),
                nowMs: fifteenMinutesOfQuiet,
                pushHealthy: true
            )
        )
    }

    func testDownPushSocketFallsBackToTodaysStalePollDegradation() {
        // Same stale timing as testFreshPresenceNeedsHealthyRelay's boundary,
        // but explicit about pushHealthy=false (the default) to document the
        // fallback path stays exactly as it was before pushHealthy existed.
        let now = 2 * ContactReachability.relayPollIntervalMs + 1
        XCTAssertFalse(
            ContactReachability.selfRelayHealthy(.ok(lastSyncMs: 0), nowMs: now, pushHealthy: false)
        )
    }

    func testHealthyPushSocketDoesNotRescueARelayHealthThatNeverActuallySucceeded() {
        // pushHealthy only overrides staleness, not a genuine last-known
        // failure/no-config/no-internet state -- health must still be .ok.
        XCTAssertFalse(
            ContactReachability.selfRelayHealthy(.failing(lastAttemptMs: 0), nowMs: 999_999, pushHealthy: true)
        )
        XCTAssertFalse(
            ContactReachability.selfRelayHealthy(.noInternet, nowMs: 999_999, pushHealthy: true)
        )
    }

    func testOnlineRelaySurvivesFifteenMinutesOfQuietWithHealthyPushSocket() {
        let fifteenMinutesOfQuiet: Int64 = 15 * 60_000
        let relayHealthy = ContactReachability.selfRelayHealthy(
            .ok(lastSyncMs: 0),
            nowMs: fifteenMinutesOfQuiet,
            pushHealthy: true
        )
        let level = ContactReachability.compute(
            directLink: false,
            presenceLastSeenMs: fifteenMinutesOfQuiet,
            selfRelayHealthy: relayHealthy,
            peerLastSeenMs: nil,
            nearbyPeerCount: 0,
            nowMs: fifteenMinutesOfQuiet
        )
        XCTAssertTrue(relayHealthy)
        XCTAssertEqual(level, .onlineRelay)
    }

    func testOnlineRelayStillDegradesOnStalePollDataOncePushSocketIsDown() {
        let now = 2 * ContactReachability.relayPollIntervalMs + 1
        let relayHealthy = ContactReachability.selfRelayHealthy(.ok(lastSyncMs: 0), nowMs: now, pushHealthy: false)
        let level = ContactReachability.compute(
            directLink: false,
            presenceLastSeenMs: now,
            selfRelayHealthy: relayHealthy,
            peerLastSeenMs: nil,
            nearbyPeerCount: 0,
            nowMs: now
        )
        XCTAssertFalse(relayHealthy)
        XCTAssertEqual(level, .offline)
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
