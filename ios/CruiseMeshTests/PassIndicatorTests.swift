import XCTest
@testable import CruiseMesh

/// The Shore Pass indicator mapping, and the projection of the core's pass
/// health fold onto this shell's `RelayHealth`. Mirrors Android's
/// PassIndicatorTest + RelayFaultPolicyTest.
///
/// The fold's precedence itself is not restated here -- it lives in
/// `core/src/session/relay_policy.rs` and reaches this file only as the vector
/// table the core exports, which the Rust and Android suites assert against
/// too. What is asserted here is what is genuinely this shell's: the display
/// mapping, and the heading rules the core does not carry.
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

    func testEveryCoreHealthVectorProjectsOntoTheShellHealth() {
        // Why an expired pass beats a successful poll, and why a suspended one
        // does not, is core policy and is pinned in
        // `core/src/session/relay_policy.rs`. Restating it here would be a
        // second source of truth on the platform whose suite cannot be run
        // without a Mac -- and the copy that could not be re-run is the copy
        // that would go stale. What is genuinely iOS's to prove is the
        // projection: every health the core can return reaches the right
        // `RelayHealth` with this shell's timestamp on it.
        for vector in coreFamilyRelayHealthVectors() {
            XCTAssertEqual(
                RelayHealth.afterSyncPass(
                    fault: vector.fault,
                    ownRelaySucceeded: vector.ownRelaySucceeded,
                    anyRelaySucceeded: vector.anyRelaySucceeded,
                    nowMs: now
                ),
                Self.expectedHealth(vector.expected, nowMs: now),
                vector.name
            )
        }
    }

    func testEveryCoreHealthHasADistinctProjection() {
        // A `case` pointing at the wrong RelayHealth would still compile and
        // would still be exhaustive. Two healths collapsing onto one display
        // state is the shape that bug takes.
        let all: [CoreRelayPassHealth] = [
            .ok, .quotaFull, .messageTooLarge, .rateLimited,
            .expired, .suspended, .tokenRejected, .failing,
        ]
        let projections = all.map { Self.expectedHealth($0, nowMs: now) }
        for (index, projection) in projections.enumerated() {
            for other in projections[(index + 1)...] {
                XCTAssertNotEqual(projection, other)
            }
        }
    }

    /// The projection asserted above, written once. Exhaustive with no
    /// `default`, so a health the core grows later stops compiling here rather
    /// than silently falling into a catch-all.
    private static func expectedHealth(_ health: CoreRelayPassHealth, nowMs: Int64) -> RelayHealth {
        switch health {
        case .ok: return .ok(lastSyncMs: nowMs)
        case .quotaFull: return .quotaFull(lastAttemptMs: nowMs)
        case .messageTooLarge: return .messageTooLarge(lastAttemptMs: nowMs)
        case .rateLimited: return .rateLimited(lastAttemptMs: nowMs)
        case .expired: return .expired(lastAttemptMs: nowMs)
        case .suspended: return .suspended(lastAttemptMs: nowMs)
        case .tokenRejected: return .tokenRejected(lastAttemptMs: nowMs)
        case .failing: return .failing(lastAttemptMs: nowMs)
        }
    }

    func testWorseFaultKeepsThePersistentCondition() {
        // Order independence is a property of the fold, and the fold is what
        // the controller calls repeatedly as a pass observes faults. Asserted
        // through the shim because that is the call the controller makes.
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
            ShorePassHeading.of(.ok(lastSyncMs: now), configured: false, lastVerdict: .ok(lastSyncMs: now)),
            .notSetUp
        )
    }

    func testHeadingReadsAsSetUpWhenTheRelayAnswersOk() {
        XCTAssertEqual(
            ShorePassHeading.of(.ok(lastSyncMs: now), configured: true, lastVerdict: nil),
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
                ShorePassHeading.of(health, configured: true, lastVerdict: .ok(lastSyncMs: now)),
                .ready,
                "\(health) is an absent answer, not a failing one"
            )
        }
    }

    func testASavedPassWithNoAnswerYetSaysItIsBeingChecked() {
        for health in [RelayHealth.checking, .noConfig] {
            XCTAssertEqual(
                ShorePassHeading.of(health, configured: true, lastVerdict: nil),
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
                ShorePassHeading.of(health, configured: true, lastVerdict: .ok(lastSyncMs: now)),
                .configured,
                "\(health) is an answer and must beat the previous OK"
            )
        }
    }

    func testACheckInFlightHoldsAFailingVerdictToo() {
        XCTAssertEqual(
            ShorePassHeading.of(.checking, configured: true, lastVerdict: .tokenRejected(lastAttemptMs: now)),
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
