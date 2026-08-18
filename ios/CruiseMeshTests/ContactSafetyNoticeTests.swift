import XCTest
@testable import CruiseMesh

/// §10.4's changed-safety-state surface: which fact is shown, which reason offers
/// a way to settle it, and that every reason has words.
///
/// The Swift twin of Android's `ContactSafetyNoticeTest`.
final class ContactSafetyNoticeTests: XCTestCase {
    private let alice = Data([0xa1, 0xa2, 0xa3])
    private let bob = Data([0xb1, 0xb2, 0xb3])

    private func fact(
        person: Data,
        reason: ContactSafetyReason,
        observedSeq: UInt64,
        acknowledged: Bool = false
    ) -> ContactSafetyFact {
        ContactSafetyFact(
            personUserId: person,
            reason: reason,
            deviceIds: [],
            recoveryEpoch: 0,
            seq: observedSeq,
            observedSeq: observedSeq,
            acknowledged: acknowledged
        )
    }

    /// Several outstanding facts for one contact collapse to the newest, by
    /// core's own monotone observation order — never by a wall clock, which
    /// nothing on the write path has a trustworthy one of.
    func testTheNewestUnacknowledgedFactWins() {
        let facts = [
            fact(person: alice, reason: .deviceRevoked, observedSeq: 4),
            fact(person: alice, reason: .rosterForked, observedSeq: 9),
            fact(person: alice, reason: .identityRecovered, observedSeq: 7),
        ]
        XCTAssertEqual(latestSafetyFact(facts: facts, personUserId: alice)?.observedSeq, 9)
    }

    func testAnotherContactsFactIsNeverShownHere() {
        let facts = [fact(person: bob, reason: .deviceRevoked, observedSeq: 3)]
        XCTAssertNil(latestSafetyFact(facts: facts, personUserId: alice))
    }

    func testAcknowledgedFactsAreNotShown() {
        let facts = [
            fact(person: alice, reason: .deviceRevoked, observedSeq: 5, acknowledged: true),
        ]
        XCTAssertNil(latestSafetyFact(facts: facts, personUserId: alice))
    }

    /// Only DL-2's fork is a state a person can settle themselves after checking
    /// out of band. The other two are things that happened; acknowledging them
    /// puts the banner away and changes nothing else.
    func testOnlyTheForkOffersTheOutOfBandCheck() {
        XCTAssertTrue(offersOutOfBandCheck(reason: .rosterForked))
        XCTAssertFalse(offersOutOfBandCheck(reason: .deviceRevoked))
        XCTAssertFalse(offersOutOfBandCheck(reason: .identityRecovered))
    }

    /// Family words, and the contact's name in every one of them. No roster, no
    /// epoch, no tombstone, no certificate.
    func testEveryReasonHasWordsThatNameTheContact() {
        for reason in [
            ContactSafetyReason.deviceRevoked,
            .identityRecovered,
            .rosterForked,
        ] {
            let text = contactSafetyCopy(reason: reason, contactName: "Christie")
            XCTAssertTrue(text.contains("Christie"), "\(reason) did not name the contact")
            for jargon in ["roster", "epoch", "tombstone", "certificate"] {
                XCTAssertFalse(
                    text.lowercased().contains(jargon),
                    "\(reason) leaked protocol jargon: \(jargon)"
                )
            }
        }
    }
}
