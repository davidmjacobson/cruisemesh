import XCTest
@testable import CruiseMesh

/// Mirrors Android's `RadioPowerPolicyTest.kt` "relayPollIntervalMs" cases,
/// plus the iOS-specific foreground/background axis (battery, 2026-07-21).
final class RelayPollPolicyTests: XCTestCase {
    func testNoPriorDecisionAndCurrentlyHealthyForegroundUsesHealthyInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: nil, currentlyHealthy: true, foreground: true),
            RelayPollPolicy.healthyForegroundMs
        )
    }

    func testNoPriorDecisionAndCurrentlyUnhealthyUsesUnhealthyInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: nil, currentlyHealthy: false, foreground: true),
            RelayPollPolicy.unhealthyOrBackgroundMs
        )
    }

    func testStayingHealthyForegroundUsesHealthyInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: true, currentlyHealthy: true, foreground: true),
            RelayPollPolicy.healthyForegroundMs
        )
    }

    func testStayingUnhealthyUsesUnhealthyInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: false, currentlyHealthy: false, foreground: true),
            RelayPollPolicy.unhealthyOrBackgroundMs
        )
    }

    func testHealthyToDownTransitionWhileForegroundUsesShortImmediateInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: true, currentlyHealthy: false, foreground: true),
            RelayPollPolicy.transitionMs
        )
    }

    func testUnhealthyToHealthyTransitionUsesHealthyIntervalNotTransitionOne() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: false, currentlyHealthy: true, foreground: true),
            RelayPollPolicy.healthyForegroundMs
        )
    }

    // -- iOS-specific: backgrounding always wins, regardless of health -----

    func testBackgroundWithHealthyPushStillUsesBackgroundInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: true, currentlyHealthy: true, foreground: false),
            RelayPollPolicy.unhealthyOrBackgroundMs
        )
    }

    func testBackgroundWithUnhealthyPushUsesBackgroundInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: false, currentlyHealthy: false, foreground: false),
            RelayPollPolicy.unhealthyOrBackgroundMs
        )
    }

    func testBackgroundSuppressesTheTransitionInterval() {
        // Even a healthy-to-down transition doesn't get the short transition
        // interval while backgrounded -- background is already at the fast
        // safety-net cadence, so there's nothing to catch up faster than.
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: true, currentlyHealthy: false, foreground: false),
            RelayPollPolicy.unhealthyOrBackgroundMs
        )
    }

    func testNoPriorDecisionInBackgroundUsesBackgroundInterval() {
        XCTAssertEqual(
            RelayPollPolicy.relayPollIntervalMs(previouslyHealthy: nil, currentlyHealthy: true, foreground: false),
            RelayPollPolicy.unhealthyOrBackgroundMs
        )
    }
}
