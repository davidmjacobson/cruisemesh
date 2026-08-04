import XCTest
@testable import CruiseMesh

/// CP2b: pins the Shore Pass indicator mapping and the sync-pass fold to
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

    func testAnExpiredPassBeatsASuccessfulPoll() {
        // relayd gives an expired family a seven-day read-only grace
        // (FAMILY_EXPIRY_GRACE_MS) in which fetch and ack still succeed and
        // only POST takes the 403 -- so this exact combination is what a
        // paying family sees for a week after their pass lapses. Folding it
        // to .ok told them the Shore Pass was working while nothing they
        // wrote left the phone.
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .passExpired, ownRelaySucceeded: true, anyRelaySucceeded: true, nowMs: now
            ),
            .expired(lastAttemptMs: now)
        )
        // Past the grace window relayd rejects reads too -- the same answer
        // by the other route.
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .passExpired, ownRelaySucceeded: false, anyRelaySucceeded: true, nowMs: now
            ),
            .expired(lastAttemptMs: now)
        )
    }

    func testTheOtherCredentialFaultsKeepPreCP2bPrecedence() {
        // Suspension and token rejection do not move up with expiry:
        // relayd's authorize_family rejects every op for both, so neither
        // can co-occur with a successful poll in the first place.
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .passSuspended, ownRelaySucceeded: true, anyRelaySucceeded: true, nowMs: now
            ),
            .ok(lastSyncMs: now)
        )
        XCTAssertEqual(
            RelayHealth.afterSyncPass(
                fault: .passSuspended, ownRelaySucceeded: false, anyRelaySucceeded: false, nowMs: now
            ),
            .suspended(lastAttemptMs: now)
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

    func testHeadingInvitesSetupWhateverTheStaleHealthSays() {
        XCTAssertEqual(
            CruisePassHeading.of(.ok(lastSyncMs: now), configured: false, lastVerdict: .ok(lastSyncMs: now)),
            .notSetUp
        )
    }

    func testHeadingReadsAsSetUpWhenTheRelayAnswersOk() {
        XCTAssertEqual(
            CruisePassHeading.of(.ok(lastSyncMs: now), configured: true, lastVerdict: nil),
            .ready
        )
    }

    func testARecheckDoesNotDemoteAPassThatJustVerified() {
        // Aunt Joan's report: pasting a card showed "Shore Pass is set up"
        // with its green check, then flipped to "Shore Pass is configured"
        // and back as the first background sync -- and the service restart
        // that clears health to .noConfig -- passed through. A check in
        // flight is not evidence against the pass, so the heading must hold.
        for health in [RelayHealth.checking, .noConfig] {
            XCTAssertEqual(
                CruisePassHeading.of(health, configured: true, lastVerdict: .ok(lastSyncMs: now)),
                .ready,
                "\(health) is an absent answer, not a failing one"
            )
        }
    }

    func testASavedPassWithNoAnswerYetSaysItIsBeingChecked() {
        for health in [RelayHealth.checking, .noConfig] {
            XCTAssertEqual(
                CruisePassHeading.of(health, configured: true, lastVerdict: nil),
                .checking,
                "\(health)"
            )
        }
    }

    func testAnyRealAnswerOtherThanOkDropsTheGreenCheckAtOnce() {
        // The stickiness above must never outlive a genuine verdict: a pass
        // the relay has started rejecting loses the check on that answer, not
        // one sync later.
        let healths: [RelayHealth] = [
            .noInternet,
            .failing(lastAttemptMs: now),
            .expired(lastAttemptMs: now),
            .suspended(lastAttemptMs: now),
            .tokenRejected(lastAttemptMs: now),
            .quotaFull(lastAttemptMs: now),
            .messageTooLarge(lastAttemptMs: now),
            .rateLimited(lastAttemptMs: now),
        ]
        for health in healths {
            XCTAssertEqual(
                CruisePassHeading.of(health, configured: true, lastVerdict: .ok(lastSyncMs: now)),
                .configured,
                "\(health) is an answer and must beat the previous OK"
            )
        }
    }

    func testACheckInFlightHoldsAFailingVerdictToo() {
        XCTAssertEqual(
            CruisePassHeading.of(.checking, configured: true, lastVerdict: .tokenRejected(lastAttemptMs: now)),
            .configured
        )
    }

    func testOnlyRealAnswersCountAsVerdicts() {
        XCTAssertFalse(RelayHealth.checking.isPassVerdict)
        XCTAssertFalse(RelayHealth.noConfig.isPassVerdict)
        let answers: [RelayHealth] = [
            .ok(lastSyncMs: now),
            .noInternet,
            .failing(lastAttemptMs: now),
            .expired(lastAttemptMs: now),
            .suspended(lastAttemptMs: now),
            .tokenRejected(lastAttemptMs: now),
            .quotaFull(lastAttemptMs: now),
            .messageTooLarge(lastAttemptMs: now),
            .rateLimited(lastAttemptMs: now),
        ]
        for health in answers {
            XCTAssertTrue(health.isPassVerdict, "\(health) is an answer")
        }
    }

    func testRetryAfterHonorsAdvertisedWindow() {
        XCTAssertEqual(relayRetryAfterMs(retryAfterHeader: "3"), 3_000)
        XCTAssertEqual(relayRetryAfterMs(retryAfterHeader: "999"), 60_000)
        XCTAssertEqual(relayRetryAfterMs(retryAfterHeader: nil), 30_000)
    }
}
