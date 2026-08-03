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

    private func candidates(recipient: Contact, contacts: [Contact]) -> [String] {
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

        // The reported symptom: a tester-pass person offered inside a family.
        XCTAssertEqual(
            candidates(recipient: cardFor("Kid", memberToken: ownToken), contacts: contacts),
            ["Sibling"]
        )
        // ...and the same leak outbound, which would hand a family's names to
        // the tester fleet.
        XCTAssertEqual(candidates(recipient: tester, contacts: contacts), [])
    }

    func testFamilyIntroductionsStillWork() {
        let contacts = [
            cardFor("Parent", memberToken: ownToken),
            cardFor("Kid1", memberToken: ownToken),
            cardFor("Kid2", memberToken: ownToken),
        ]
        XCTAssertEqual(
            candidates(recipient: contacts[1], contacts: contacts),
            ["Parent", "Kid2"]
        )
    }

    func testFamilyMemberWithoutAPassYetStaysEligible() {
        // Their card carries no relay fields, so our sends to them already
        // land in our own mailbox. Excluding them would break introductions
        // for exactly the half-onboarded family the feature helps most.
        let noPass = contact("NotSetUpYet")
        let parent = cardFor("Parent", memberToken: ownToken)
        XCTAssertTrue(FriendDirectoryScope.sharesOwnPass(noPass, ownRelay: ownRelay))
        XCTAssertEqual(candidates(recipient: parent, contacts: [noPass, parent]), ["NotSetUpYet"])
    }

    func testPreCp4CardCarryingTheMemberTokenIsStillOurFamily() {
        let legacy = contact("Legacy", relayUrl: relayUrl, relayToken: ownToken)
        XCTAssertTrue(FriendDirectoryScope.sharesOwnPass(legacy, ownRelay: ownRelay))
    }

    func testSameFamilyTokenOnADifferentRelayHostIsNotOurPass() {
        let elsewhere = contact("Elsewhere", relayUrl: "https://other.example", relayToken: ownToken)
        XCTAssertFalse(FriendDirectoryScope.sharesOwnPass(elsewhere, ownRelay: ownRelay))
    }

    func testRecipientIsNeverOfferedThemselves() {
        let selfContact = cardFor("Kid", memberToken: ownToken)
        XCTAssertEqual(candidates(recipient: selfContact, contacts: [selfContact]), [])
    }

    func testWithNoPassOfOurOwnNobodyIsExcluded() {
        // Nothing to compare against; silently emptying every snapshot would
        // switch the feature off for anyone who has not bought a pass.
        let tester = cardFor("Tester", memberToken: testerToken)
        let other = cardFor("Other", memberToken: testerToken)
        XCTAssertEqual(
            FriendDirectoryScope.candidatesFor(
                recipient: tester,
                contacts: [tester, other],
                ownRelay: nil
            ).map(\.name),
            ["Other"]
        )
    }
}
