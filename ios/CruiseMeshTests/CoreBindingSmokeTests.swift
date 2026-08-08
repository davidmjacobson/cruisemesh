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
/// answers mean* is tested in `core/src/connection_health.rs` and must not be
/// restated here. `CoreBindingSmokeTest.kt` is the same file for the other
/// shell.
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
