import XCTest
@testable import CruiseMesh

/// The *shape* of the UniFFI boundary, not the policy behind it.
///
/// The checked-in Swift bindings are held to the core by a drift gate that
/// regenerates and diffs them (`rust.yml`), which proves the files match. It
/// cannot prove the generated marshalling carries a value across intact --
/// UniFFI verifies its per-function checksums at *runtime*, and enum
/// discriminants, optional fields, byte arrays, and nested records each have
/// their own lowering path that only executing them exercises. On iOS that
/// execution happens nowhere else in CI.
///
/// So these assert only what a marshalling bug would break: every variant of
/// an enum survives a trip through Rust, an absent optional stays absent and a
/// present one keeps its value, bytes come back byte-equal, and a record's
/// fields land in the fields they left from. Every rule about *what the
/// answers mean* is tested in the core module that owns it
/// (`core/src/connection_health.rs`, `core/src/session/relay_policy.rs`) and
/// must not be restated here. `CoreBindingSmokeTest.kt` is the same file for
/// the other shell.
///
/// The reverse is true too, and matters more: this is not a second drift
/// check. `ios.yml` regenerates `Generated/` in `core/build-ios.sh` before
/// `xcodebuild` runs, so this suite always exercises freshly generated
/// bindings and never the committed ones. It catches marshalling bugs; only
/// the `rust.yml` diff catches a checked-in binding going stale.
final class CoreBindingSmokeTests: XCTestCase {
    /// Fixed instant; nothing here depends on which one.
    private let now: Int64 = 1_760_000_000_000

    // MARK: - Enum discriminants

    /// Every declared variant lowers into Rust and comes back distinguishable.
    /// A discriminant that shifted by one lands out of range and traps in
    /// Rust; one that collided returns a duplicate rank.
    func testEveryAttentionVariantCrossesTheBoundaryDistinctly() {
        let ranks = Self.allAttentionCases.map { corePersonAttentionRank(attention: $0) }
        XCTAssertEqual(Set(ranks).count, Self.allAttentionCases.count)
    }

    /// A second enum, lowered through a different signature: every declared
    /// case reaches Rust as a discriminant Rust recognises, so a shifted one
    /// lands out of range and traps rather than answering.
    ///
    /// Deliberately not asserted: *which* cases answer which way. Which
    /// reaches count as reachable is policy, owned and pinned by
    /// `core/src/connection_health.rs`; restating it here would turn a future
    /// policy change into a red marshalling test. Only that the answers are
    /// not all identical, which is what a total discriminant collapse would
    /// look like from this side.
    func testEveryReachVariantLowersIntoADiscriminantRustRecognises() {
        let answers = Self.allReachCases.map { corePersonIsReachableNow(reach: $0) }
        XCTAssertEqual(answers.count, Self.allReachCases.count)
        XCTAssertEqual(Set(answers).count, 2)
    }

    // MARK: - Optional fields

    /// Three distinct answers: the absent form is not confused with a present
    /// one, and two different present values are not confused with each other
    /// -- so the payload of an optional is genuinely carried, not just its
    /// presence. Which link maps to which reach is the core's business, so it
    /// is the distinctness that is asserted and not the mapping.
    func testAnOptionalArgumentCarriesBothItsAbsentAndPresentForms() {
        let absent = corePersonReach(directLink: nil, presenceLastSeenMs: 0, ownRelayUsable: false, nowMs: now)
        let bluetooth = corePersonReach(directLink: .bluetooth, presenceLastSeenMs: 0, ownRelayUsable: false, nowMs: now)
        let localWifi = corePersonReach(directLink: .localWifi, presenceLastSeenMs: 0, ownRelayUsable: false, nowMs: now)
        XCTAssertEqual(Set([absent, bluetooth, localWifi]).count, 3)
    }

    // MARK: - Byte arrays and record round trips

    /// `userId` is passed in and echoed back untouched, so both directions of
    /// the byte converter are on the hook. The bytes deliberately include
    /// `0x00`, `0x80`, and `0xFF`: a converter that treated them as signed, as
    /// text, or as NUL-terminated would corrupt exactly those. The second
    /// person also pins an optional record field arriving unset.
    func testByteArrayAndOptionalRecordFieldsSurviveTheRoundTrip() {
        let awkward = Data([0x00, 0x7F, 0x80, 0xFF, 0x01])
        let empty = Data()
        let groups = coreGroupPeople(
            people: [
                person(userId: awkward, displayName: "Awkward", attention: .setupRejected),
                person(userId: empty, displayName: "Empty", attention: nil),
            ],
            ownRelayUsable: false,
            nowMs: now
        )
        let placements = groups.needsAttention + groups.reachableNow + groups.otherPeople
        XCTAssertEqual(placements.count, 2)
        let set = placements.first { !$0.userId.isEmpty }
        let unset = placements.first { $0.userId.isEmpty }
        XCTAssertEqual(set?.userId, awkward)
        XCTAssertEqual(set?.attention, CorePersonAttention.setupRejected)
        XCTAssertEqual(unset?.userId, empty)
        XCTAssertNil(unset?.attention)
    }

    /// A record in, a record with a nested record out. The three counts are
    /// copied verbatim by the core, so they pin unsigned marshalling in both
    /// directions -- including a value with its top bit set, which a converter
    /// that used a signed 32-bit integer would deliver as a negative number.
    func testNestedRecordFieldsLandWhereTheyLeftFrom() {
        let report = coreClassifyConnectionHealth(
            input: CoreConnectionHealthInput(
                runtime: .active,
                bluetooth: .available,
                bluetoothLinks: 3,
                localWifi: .available,
                localWifiLinks: UInt32.max,
                relay: .notSetUp,
                validatedInternet: true,
                nearbyFriendCount: 7,
                checkingSinceMs: 0,
                nowMs: now
            )
        )
        XCTAssertEqual(report.evidence.bluetoothLinks, 3)
        XCTAssertEqual(report.evidence.localWifiLinks, UInt32.max)
        XCTAssertEqual(report.evidence.nearbyFriendCount, 7)
    }

    // MARK: - Relay policy shapes

    // `core/src/session/relay_policy.rs` added shapes none of the tests above
    // reach: two objects that hold state across calls, two enums that are
    // returned rather than passed, an optional enum *argument*, and a bare
    // byte-array argument. The vector suites in FamilyRelayBackpressureTests /
    // PassIndicatorTests do execute these lowering paths today, but they
    // execute them by reading an exported table -- so if those tables are ever
    // moved behind a feature or trimmed, the coverage leaves with them. These
    // do not read a table, and so they stay.
    //
    // Same rule as everything above: nothing here asserts which answer means
    // what. That is `RATE-01`, pinned in the core.

    /// A `uniffi::Object` that holds state on the Rust side: the handle must
    /// survive between calls, or the second reservation would answer as if it
    /// were the first. Says nothing about the interval, only that the two
    /// calls reached the same instance.
    func testAnObjectHandleCarriesRustSideStateBetweenCalls() {
        let pacer = CoreFamilyRelayPacer()
        let first = pacer.reserve(nowMs: 0)
        let second = pacer.reserve(nowMs: 0)
        XCTAssertNotEqual(first, second)
    }

    /// The other object, plus the unsigned counter it returns: a fresh
    /// instance starts at zero, one call moves it, and the reset call moves it
    /// back -- so `UInt32` crosses in the returning direction and the handle
    /// is genuinely per-instance rather than shared.
    func testObjectStateAdvancesAndResetsThroughTheBoundary() {
        let backoff = CoreFamilyRelayBackoff()
        XCTAssertEqual(backoff.consecutiveRateLimits(), 0)
        _ = backoff.onRateLimited(retryAfterMs: 0, identityPublicBytes: Data())
        XCTAssertEqual(backoff.consecutiveRateLimits(), 1)
        XCTAssertEqual(CoreFamilyRelayBackoff().consecutiveRateLimits(), 0)
        backoff.onSuccessfulPass()
        XCTAssertEqual(backoff.consecutiveRateLimits(), 0)
    }

    /// An *optional enum argument* -- a lowering path no other smoke here
    /// covers -- and an enum returned by value. The absent form must not be
    /// confused with a present one, and a discriminant that shifted would land
    /// out of range in Rust and trap rather than answer.
    func testAnOptionalEnumArgumentLowersInBothItsForms() {
        let answers = [
            coreRelayPassHealth(fault: nil, ownRelaySucceeded: false, anyRelaySucceeded: false),
            coreRelayPassHealth(fault: .mailboxFull, ownRelaySucceeded: false, anyRelaySucceeded: false),
            coreRelayPassHealth(fault: .tokenRejected, ownRelaySucceeded: false, anyRelaySucceeded: false),
        ]
        XCTAssertEqual(Set(answers).count, 3)
    }

    /// Every declared case of the returned enum is reachable, so none of the
    /// eight discriminants lifts onto another's name. Which input produces
    /// which is the core's business and is not asserted; only that the set is
    /// covered.
    func testEveryPassHealthCaseLiftsBackDistinctly() {
        var faults: [CoreRelayFault?] = [nil]
        for fault in Self.allRelayFaultCases { faults.append(fault) }
        var produced = Set<CoreRelayPassHealth>()
        for fault in faults {
            for own in [false, true] {
                for any in [false, true] {
                    produced.insert(
                        coreRelayPassHealth(fault: fault, ownRelaySucceeded: own, anyRelaySucceeded: any)
                    )
                }
            }
        }
        XCTAssertEqual(produced, Set(Self.allPassHealthCases))
    }

    /// The third enum, lifted through a different signature.
    func testEveryRerunActionLiftsBackDistinctly() {
        var produced = Set<CoreRelayRerunAction>()
        for pending in [false, true] {
            for canSync in [false, true] {
                for remaining in [Int64(-1), 0, 30_000] {
                    produced.insert(
                        coreRelayRerunAction(
                            pendingRequested: pending, canSync: canSync, backoffRemainingMs: remaining
                        )
                    )
                }
            }
        }
        XCTAssertEqual(produced, Set(Self.allRerunActionCases))
    }

    /// A bare `Data` argument, with the bytes a broken converter corrupts:
    /// `0x00`, `0x80`, `0xFF`. Reversing them must change the answer, which a
    /// buffer that arrived truncated, NUL-terminated, or empty could not do.
    func testABareByteArrayArgumentCrossesIntact() {
        let awkward = Data([0x00, 0x7F, 0x80, 0xFF, 0x01])
        let reversed = Data(awkward.reversed())
        XCTAssertNotEqual(
            coreFamilyRelayJitterMs(identityPublicBytes: awkward),
            coreFamilyRelayJitterMs(identityPublicBytes: reversed)
        )
        XCTAssertEqual(
            coreFamilyRelayJitterMs(identityPublicBytes: awkward),
            coreFamilyRelayJitterMs(identityPublicBytes: Data(awkward))
        )
        // Empty is a value, not a missing argument.
        XCTAssertEqual(
            coreFamilyRelayJitterMs(identityPublicBytes: Data()),
            coreFamilyRelayJitterMs(identityPublicBytes: Data())
        )
    }

    /// `UInt64` in both directions, at a value a signed 64-bit converter would
    /// deliver as -1. Asserts only that the top bit survived the trip, not
    /// what the arithmetic did with it.
    func testAnUnsigned64BitValueKeepsItsTopBitAcrossTheBoundary() {
        let answer = coreFamilyRelayBackoffDelayMs(
            retryAfterMs: UInt64.max, consecutiveRateLimits: 1, jitterMs: 0
        )
        XCTAssertGreaterThan(answer, UInt64(Int64.max), "top bit lost")
    }

    // MARK: - Helpers

    /// Swift's generated enums are not `CaseIterable`, so the lists are
    /// written out -- and then walked by an exhaustive `switch` with no
    /// `default`, which stops compiling the moment the core grows a variant
    /// nobody added here.
    private static let allAttentionCases: [CorePersonAttention] = {
        let all: [CorePersonAttention] = [.delayed, .messageTooLarge, .passBlocked, .setupRejected]
        for value in all {
            switch value {
            case .delayed, .messageTooLarge, .passBlocked, .setupRejected: break
            }
        }
        return all
    }()

    private static let allReachCases: [CorePersonReach] = {
        let all: [CorePersonReach] = [.directBluetooth, .directLocalWifi, .relayPresence, CorePersonReach.none]
        for value in all {
            switch value {
            case .directBluetooth, .directLocalWifi, .relayPresence, .none: break
            }
        }
        return all
    }()

    private static let allRelayFaultCases: [CoreRelayFault] = {
        let all: [CoreRelayFault] = [
            .passExpired, .passSuspended, .tokenRejected,
            .mailboxFull, .messageTooLarge, .rateLimited, .outage,
        ]
        for value in all {
            switch value {
            case .passExpired, .passSuspended, .tokenRejected,
                 .mailboxFull, .messageTooLarge, .rateLimited, .outage: break
            }
        }
        return all
    }()

    private static let allPassHealthCases: [CoreRelayPassHealth] = {
        let all: [CoreRelayPassHealth] = [
            .ok, .quotaFull, .messageTooLarge, .rateLimited,
            .expired, .suspended, .tokenRejected, .failing,
        ]
        for value in all {
            switch value {
            case .ok, .quotaFull, .messageTooLarge, .rateLimited,
                 .expired, .suspended, .tokenRejected, .failing: break
            }
        }
        return all
    }()

    private static let allRerunActionCases: [CoreRelayRerunAction] = {
        let all: [CoreRelayRerunAction] = [.runAgain, .scheduleRateLimitRetry, .stop]
        for value in all {
            switch value {
            case .runAgain, .scheduleRateLimitRetry, .stop: break
            }
        }
        return all
    }()

    /// `Float` in both directions, and a record nested inside a record beside
    /// an enum. The voice-capture plan is the first exported surface here to
    /// carry a float at all, so this executes the lowering rather than
    /// trusting it.
    func testAFloatCrossesTheBoundaryAndLandsInANestedRecord() {
        let plan = voiceCapturePlan()
        XCTAssertGreaterThan(plan.cancelSlideDp, 1)

        let holding = voiceCapturePress(state: voiceCaptureIdleState()).state
        XCTAssertEqual(holding.phase, .holding)
        XCTAssertTrue(voiceCaptureDrag(state: holding, dx: -(plan.cancelSlideDp + 1), dy: 0).state.cancelArmed)
        XCTAssertFalse(voiceCaptureDrag(state: holding, dx: -(plan.cancelSlideDp - 1), dy: 0).state.cancelArmed)
    }

    private func person(
        userId: Data,
        displayName: String,
        attention: CorePersonAttention?
    ) -> CorePersonHealthInput {
        CorePersonHealthInput(
            userId: userId,
            displayName: displayName,
            blocked: false,
            directLink: nil,
            presenceLastSeenMs: 0,
            lastSeenMs: 0,
            attention: attention,
            attentionSinceMs: 0
        )
    }
}
