import XCTest
@testable import CruiseMesh

/// The pacing and 429 backoff policy lives in the core
/// (`core/src/session/relay_policy.rs`), and its formulas are pinned there.
/// What this file proves is the other half: that iOS reaches that policy
/// through the FFI and gets the same answers back.
///
/// So every expectation here comes from a vector table the core exports rather
/// than from a number typed into this file. The Rust suite and the Android JVM
/// suite assert the same tables, so a `Data` or integer-width bug in either
/// binding shows up as a vector mismatch instead of as a platform test that
/// quietly asserts something slightly different.
final class FamilyRelayBackpressureTests: XCTestCase {

    func testShimPacerReproducesTheCoreReservationSequence() {
        // One pacer, rows applied in order: the state carried between them is
        // the point, and it is state held on the far side of the FFI.
        let pacer = FamilyRelayRequestPacer()
        for vector in coreFamilyRelayPacerVectors() {
            XCTAssertEqual(pacer.reserve(nowMs: vector.nowMs), vector.expectedWaitMs, vector.name)
        }
    }

    func testBackoffCurveCrossesTheFFIUnchanged() {
        for vector in coreFamilyRelayBackoffVectors() {
            XCTAssertEqual(
                coreFamilyRelayBackoffDelayMs(
                    retryAfterMs: vector.retryAfterMs,
                    consecutiveRateLimits: vector.consecutiveRateLimits,
                    jitterMs: vector.jitterMs
                ),
                vector.expectedDelayMs,
                vector.name
            )
        }
    }

    func testIdentityBytesCrossTheFFIUnchanged() {
        // A `Data` marshalled wrong -- truncated, reordered, or handed over as
        // an empty buffer -- would still produce a plausible-looking offset.
        // Only comparing it to the core's own answer catches that.
        for vector in coreFamilyRelayJitterVectors() {
            XCTAssertEqual(
                coreFamilyRelayJitterMs(identityPublicBytes: vector.identityPublicBytes),
                vector.expectedJitterMs,
                vector.name
            )
        }
    }

    func testShimComposesTheCurveWithTheIdentityOffset() {
        // The one thing the shim adds beyond forwarding: it hands the core an
        // identity and gets back a window that already includes that
        // identity's offset. Pinned against the two pieces computed
        // separately, so the composition cannot silently drop the jitter.
        let identity = Data((0..<32).map { UInt8($0) })
        let backoff = FamilyRelayBackoff()
        let jitterMs = coreFamilyRelayJitterMs(identityPublicBytes: identity)

        let first = backoff.onRateLimited(retryAfterMs: 1_000, identityPublicBytes: identity)
        XCTAssertEqual(
            first,
            coreFamilyRelayBackoffDelayMs(
                retryAfterMs: 1_000,
                consecutiveRateLimits: 1,
                jitterMs: jitterMs
            )
        )
        XCTAssertEqual(backoff.consecutiveRateLimits, 1)

        let second = backoff.onRateLimited(retryAfterMs: 1_000, identityPublicBytes: identity)
        XCTAssertEqual(
            second,
            coreFamilyRelayBackoffDelayMs(
                retryAfterMs: 1_000,
                consecutiveRateLimits: 2,
                jitterMs: jitterMs
            )
        )
        XCTAssertEqual(backoff.consecutiveRateLimits, 2)

        backoff.onSuccessfulPass()
        XCTAssertEqual(backoff.consecutiveRateLimits, 0)
        XCTAssertEqual(
            backoff.onRateLimited(retryAfterMs: 1_000, identityPublicBytes: identity),
            first
        )
    }

    func testRerunVectorsCrossTheFFIUnchanged() {
        // `finishRelaySync` switches on this, including the storm case the
        // rule exists for. An enum discriminant that arrived scrambled would
        // land the pass in the wrong branch without failing to compile.
        var seen = Set<String>()
        for vector in coreFamilyRelayRerunVectors() {
            let action = coreRelayRerunAction(
                pendingRequested: vector.pendingRequested,
                canSync: vector.canSync,
                backoffRemainingMs: vector.backoffRemainingMs
            )
            XCTAssertEqual(action, vector.expected, vector.name)
            seen.insert("\(action)")
        }
        XCTAssertEqual(seen.count, 3, "all three rerun branches must be reachable")
    }

    func testHealthVectorsProjectOntoTheShellHealth() {
        let nowMs: Int64 = 1_800_000_000_000
        for vector in coreFamilyRelayHealthVectors() {
            XCTAssertEqual(
                RelayHealth.afterSyncPass(
                    fault: vector.fault,
                    ownRelaySucceeded: vector.ownRelaySucceeded,
                    anyRelaySucceeded: vector.anyRelaySucceeded,
                    nowMs: nowMs
                ),
                Self.expectedHealth(vector.expected, nowMs: nowMs),
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
        let projections = all.map { Self.expectedHealth($0, nowMs: 1) }
        for (index, projection) in projections.enumerated() {
            for other in projections[(index + 1)...] {
                XCTAssertNotEqual(projection, other)
            }
        }
    }

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

    func testConcurrentReservationsNeverCollideOnOneSlot() {
        // The controller paces from a detached task while other work runs. Two
        // callers must get two slots, not the same one twice -- the threading
        // property the shim exists to keep, and not something the core's own
        // single-threaded tests can show.
        let pacer = FamilyRelayRequestPacer()
        let lock = NSLock()
        var waits: [Int64] = []
        DispatchQueue.concurrentPerform(iterations: 8) { _ in
            let wait = pacer.reserve(nowMs: 0)
            lock.lock()
            waits.append(wait)
            lock.unlock()
        }
        XCTAssertEqual(Set(waits).count, 8)
        XCTAssertEqual(waits.sorted(), [0, 500, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500])
    }
}
