import XCTest
@testable import CruiseMesh

/// CP2b: pins the Cruise Pass indicator mapping and the sync-pass fold to
/// the core's transient/persistent classification, mirroring Android's
/// PassIndicatorTest + RelayFaultPolicyTest.
final class PassIndicatorTests: XCTestCase {
    private let now: Int64 = 1_800_000_000_000

    func testUnconfiguredPhoneShowsNothing() {
        // The free tier is not a fault: nearby delivery works without a
        // pass, so an un-configured phone must not wear an error mark.
        let healths: [RelayHealth] = [
            .noConfig, .checking, .noInternet,
            .ok(lastSyncMs: now),
            .failing(lastAttemptMs: now),
            .expired(lastAttemptMs: now),
            .suspended(lastAttemptMs: now),
            .tokenRejected(lastAttemptMs: now),
            .quotaFull(lastAttemptMs: now),
            .messageTooLarge(lastAttemptMs: now),
            .rateLimited(lastAttemptMs: now),
        ]
        for health in healths {
            XCTAssertEqual(PassIndicator.of(health, configured: false), .none, "\(health)")
        }
    }

    func testSelfHealingConditionsAreTheQuietQuestionMarkState() {
        // David's UX spec: can't-reach-right-now and rate-limited both clear
        // on their own, so they share the transient "?" state.
        for health in [RelayHealth.failing(lastAttemptMs: now), .rateLimited(lastAttemptMs: now)] {
            XCTAssertEqual(PassIndicator.of(health, configured: true), .attention, "\(health)")
        }
        XCTAssertEqual(PassIndicator.attention.systemImage, "questionmark.circle.fill")
    }

    func testStatesThatNeedTheUserToActAreRed() {
        let healths: [RelayHealth] = [
            .expired(lastAttemptMs: now),
            .suspended(lastAttemptMs: now),
            .tokenRejected(lastAttemptMs: now),
            .quotaFull(lastAttemptMs: now),
            .messageTooLarge(lastAttemptMs: now),
        ]
        for health in healths {
            XCTAssertEqual(PassIndicator.of(health, configured: true), .actionRequired, "\(health)")
        }
        XCTAssertEqual(PassIndicator.actionRequired.systemImage, "exclamationmark.circle.fill")
    }

    func testMailboxFaultsBeatASuccessfulPoll() {
        // relayd keeps serving fetches while rejecting posts, so quota /
        // oversized / rate-limited surface even when the pass succeeded.
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .mailboxFull, ownRelaySucceeded: true, anyRelaySucceeded: true, nowMs: now
            ),
            .quotaFull(lastAttemptMs: now)
        )
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .rateLimited, ownRelaySucceeded: true, anyRelaySucceeded: true, nowMs: now
            ),
            .rateLimited(lastAttemptMs: now)
        )
    }

    func testCredentialFaultsKeepPreCP2bPrecedence() {
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .passExpired, ownRelaySucceeded: true, anyRelaySucceeded: true, nowMs: now
            ),
            .ok(lastSyncMs: now)
        )
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .passExpired, ownRelaySucceeded: false, anyRelaySucceeded: true, nowMs: now
            ),
            .expired(lastAttemptMs: now)
        )
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .tokenRejected, ownRelaySucceeded: false, anyRelaySucceeded: false, nowMs: now
            ),
            .tokenRejected(lastAttemptMs: now)
        )
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: nil, ownRelaySucceeded: false, anyRelaySucceeded: true, nowMs: now
            ),
            .failing(lastAttemptMs: now)
        )
    }

    func testWorseFaultKeepsThePersistentCondition() {
        var fault: CoreRelayFault?
        fault = RelayHealth.worseFault(fault, .rateLimited)
        fault = RelayHealth.worseFault(fault, .mailboxFull)
        XCTAssertEqual(fault, .mailboxFull)
        fault = nil
        fault = RelayHealth.worseFault(fault, .mailboxFull)
        fault = RelayHealth.worseFault(fault, .rateLimited)
        XCTAssertEqual(fault, .mailboxFull)
    }

    func testIndicatorBucketsAgreeWithTheCoreTransientSplit() {
        let faults: [CoreRelayFault] = [
            .passExpired, .passSuspended, .tokenRejected,
            .mailboxFull, .messageTooLarge, .rateLimited,
        ]
        for fault in faults {
            let health = RelayHealth.afterSyncPass(
                fault: fault, ownRelaySucceeded: false, anyRelaySucceeded: false, nowMs: now
            )
            let indicator = PassIndicator.of(health, configured: true)
            if relayFaultIsTransient(fault: fault) {
                XCTAssertEqual(indicator, .attention, "\(fault) is transient and must stay amber")
            } else {
                XCTAssertEqual(indicator, .actionRequired, "\(fault) is persistent and must demand action")
            }
        }
    }

    func testRetryAfterHonorsAdvertisedWindow() {
        XCTAssertEqual(relayRetryAfterMs(retryAfterHeader: "3"), 3_000)
        XCTAssertEqual(relayRetryAfterMs(retryAfterHeader: "999"), 60_000)
        XCTAssertEqual(relayRetryAfterMs(retryAfterHeader: nil), 30_000)
    }
}
