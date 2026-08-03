import XCTest
@testable import CruiseMesh

/// Mirrors Android's FriendDirectoryScopeTest.kt.
final class FriendDirectoryScopeTests: XCTestCase {
    private let relayUrl = "https://relay.example"
    private let ownToken = "family-member-token"
    private let testerToken = "tester-pass-member-token"

    private var ownRelay: RelayConfig {
        RelayConfig(relayUrl: relayUrl, relayToken: ownToken)
    }

    private func contact(_ name: String, relayUrl: String? = nil, relayToken: String? = nil) -> Contact {
        Contact(
            userId: Data(name.padding(toLength: 16, withPad: ".", startingAt: 0).utf8),
            name: name,
            signPk: Data(repeating: 1, count: 32),
            agreePk: Data(repeating: 2, count: 32),
            relayUrl: relayUrl,
            relayToken: relayToken,
            nickname: nil
        )
    }

    /// A card as it is actually issued post-CP4: the family's deposit token.
    private func cardFor(_ name: String, memberToken: String) -> Contact {
        contact(name, relayUrl: relayUrl, relayToken: relayDepositTokenFor(memberToken: memberToken))
    }

    private func candidates(
        recipient: Contact,
        contacts: [Contact],
        ownRelay: RelayConfig?,
        addedNearby: @escaping (Data) -> Bool = { _ in true }
    ) -> [String] {
        FriendDirectoryScope.candidatesFor(
            recipient: recipient,
            contacts: contacts,
            ownRelay: ownRelay,
            addedNearby: addedNearby
        ).map(\.name)
    }

    func testContactOnAnotherPassIsNeverOfferedAndNeverReceives() {
        let family = cardFor("Sibling", memberToken: ownToken)
        let tester = cardFor("Tester", memberToken: testerToken)
        let contacts = [family, tester]

        XCTAssertEqual(
            candidates(
                recipient: cardFor("Kid", memberToken: ownToken),
                contacts: contacts,
                ownRelay: ownRelay
            ),
            ["Sibling"]
        )
        XCTAssertEqual(candidates(recipient: tester, contacts: contacts, ownRelay: ownRelay), [])
    }

    func testFamilyIntroductionsStillWork() {
        let contacts = [
            cardFor("Parent", memberToken: ownToken),
            cardFor("Kid1", memberToken: ownToken),
            cardFor("Kid2", memberToken: ownToken),
        ]
        XCTAssertEqual(
            candidates(recipient: contacts[1], contacts: contacts, ownRelay: ownRelay),
            ["Parent", "Kid2"]
        )
    }

    func testHolidayAcquaintanceWithoutAPassIsNotFamilyEvenMetInPerson() {
        // The cruise case: another family's kid, scanned face to face, no pass
        // of their own. Being nearby must not buy an exception -- that is
        // exactly how a relative mid-onboarding looks.
        let outsider = contact("CruiseKid")
        let parent = cardFor("Parent", memberToken: ownToken)
        XCTAssertFalse(
            FriendDirectoryScope.introducible(outsider, ownRelay: ownRelay, addedNearby: true)
        )
        XCTAssertEqual(
            candidates(recipient: parent, contacts: [outsider, parent], ownRelay: ownRelay),
            []
        )
    }

    func testFamilyMemberJoiningOurPassBecomesEligibleAtThatMoment() {
        let before = contact("NotSetUpYet")
        let after = cardFor("NotSetUpYet", memberToken: ownToken)
        XCTAssertFalse(FriendDirectoryScope.introducible(before, ownRelay: ownRelay, addedNearby: true))
        XCTAssertTrue(FriendDirectoryScope.introducible(after, ownRelay: ownRelay, addedNearby: false))
    }

    func testWithNoPassAtAllMeetingInPersonIsTheOnlyBoundaryLeft() {
        let met = contact("Met")
        let neverMet = contact("NeverMet")
        XCTAssertEqual(
            candidates(recipient: met, contacts: [met, neverMet], ownRelay: nil),
            ["NeverMet"]
        )
        XCTAssertEqual(
            candidates(
                recipient: met,
                contacts: [met, neverMet],
                ownRelay: nil,
                addedNearby: { _ in false }
            ),
            []
        )
    }

    func testWithoutAPassAContactWhoHasOneBelongsToAFamilyWeCannotSee() {
        let passHolder = cardFor("HasPass", memberToken: testerToken)
        XCTAssertFalse(
            FriendDirectoryScope.introducible(passHolder, ownRelay: nil, addedNearby: true)
        )
    }

    func testPreCp4CardCarryingTheMemberTokenIsStillOurFamily() {
        let legacy = contact("Legacy", relayUrl: relayUrl, relayToken: ownToken)
        XCTAssertTrue(FriendDirectoryScope.introducible(legacy, ownRelay: ownRelay, addedNearby: false))
    }

    func testSameFamilyTokenOnADifferentRelayHostIsNotOurPass() {
        let elsewhere = contact("Elsewhere", relayUrl: "https://other.example", relayToken: ownToken)
        XCTAssertFalse(
            FriendDirectoryScope.introducible(elsewhere, ownRelay: ownRelay, addedNearby: true)
        )
    }

    func testRecipientIsNeverOfferedThemselves() {
        let selfContact = cardFor("Kid", memberToken: ownToken)
        XCTAssertEqual(
            candidates(recipient: selfContact, contacts: [selfContact], ownRelay: ownRelay),
            []
        )
    }
}
