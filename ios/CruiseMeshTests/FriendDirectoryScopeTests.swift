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
        ownRelay: RelayConfig?
    ) -> [String] {
        FriendDirectoryScope.candidatesFor(
            recipient: recipient,
            contacts: contacts,
            ownRelay: ownRelay
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

    func testContactWithoutAPassIsNotIntroducibleHoweverWeMetThem() {
        // The holiday-acquaintance case and the relative-mid-onboarding case
        // look identical from the card, so neither is introduced. There is
        // deliberately no in-person exception.
        let outsider = contact("CruiseKid")
        let parent = cardFor("Parent", memberToken: ownToken)
        XCTAssertFalse(FriendDirectoryScope.introducible(outsider, ownRelay: ownRelay))
        XCTAssertEqual(
            candidates(recipient: parent, contacts: [outsider, parent], ownRelay: ownRelay),
            []
        )
    }

    func testFamilyMemberJoiningOurPassBecomesEligibleAtThatMoment() {
        // The pass-change re-fan is what replays this without user action.
        let before = contact("NotSetUpYet")
        let after = cardFor("NotSetUpYet", memberToken: ownToken)
        XCTAssertFalse(FriendDirectoryScope.introducible(before, ownRelay: ownRelay))
        XCTAssertTrue(FriendDirectoryScope.introducible(after, ownRelay: ownRelay))
    }

    func testWithNoPassOfOurOwnNobodyIsIntroducibleAtAll() {
        // No family boundary is drawn, so no transitive introduction happens;
        // people scan a code or share their own friend link instead.
        let met = contact("Met")
        let other = contact("Other")
        XCTAssertEqual(candidates(recipient: met, contacts: [met, other], ownRelay: nil), [])
        XCTAssertFalse(
            FriendDirectoryScope.introducible(
                cardFor("HasPass", memberToken: testerToken),
                ownRelay: nil
            )
        )
    }

    func testPreCp4CardCarryingTheMemberTokenIsStillOurFamily() {
        let legacy = contact("Legacy", relayUrl: relayUrl, relayToken: ownToken)
        XCTAssertTrue(FriendDirectoryScope.introducible(legacy, ownRelay: ownRelay))
    }

    func testSameFamilyTokenOnADifferentRelayHostIsNotOurPass() {
        let elsewhere = contact("Elsewhere", relayUrl: "https://other.example", relayToken: ownToken)
        XCTAssertFalse(FriendDirectoryScope.introducible(elsewhere, ownRelay: ownRelay))
    }

    func testRecipientIsNeverOfferedThemselves() {
        let selfContact = cardFor("Kid", memberToken: ownToken)
        XCTAssertEqual(
            candidates(recipient: selfContact, contacts: [selfContact], ownRelay: ownRelay),
            []
        )
    }
}
