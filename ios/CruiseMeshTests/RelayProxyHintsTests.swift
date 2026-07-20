import XCTest
@testable import CruiseMesh

/// FI2: `relayProxyHints` is the pure hint-set computation behind relay
/// proxy-polling -- mirrors Android's `MeshService.relayProxyHints`. It has
/// no store/network access, so it's directly unit-testable without an
/// `AppStore` fixture.
final class RelayProxyHintsTests: XCTestCase {
    private func makeContact(userId: Data, name: String) -> Contact {
        Contact(
            userId: userId,
            name: name,
            signPk: Data(repeating: 0xAA, count: 32),
            agreePk: Data(repeating: 0xBB, count: 32),
            relayUrl: nil,
            relayToken: nil
        )
    }

    func testIncludesRecentDayHintsForEveryOtherContact() {
        let ownUserId = Data([1])
        let alice = makeContact(userId: Data([2]), name: "Alice")
        let bob = makeContact(userId: Data([3]), name: "Bob")
        let now: Int64 = 1_700_000_000_000

        let hints = relayProxyHints(contacts: [alice, bob], ownUserId: ownUserId, nowMs: now)

        let expectedAlice = Set((0...MeshDefaults.carryHintDayWindow).map { daysAgo in
            computeRecipientHint(recipientUserId: alice.userId, timestampMs: now - daysAgo * MeshDefaults.msPerDay)
        })
        let expectedBob = Set((0...MeshDefaults.carryHintDayWindow).map { daysAgo in
            computeRecipientHint(recipientUserId: bob.userId, timestampMs: now - daysAgo * MeshDefaults.msPerDay)
        })

        XCTAssertEqual(Set(hints), expectedAlice.union(expectedBob))
        // One entry per day in the window, per contact -- no accidental dedup
        // or day-window drift.
        XCTAssertEqual(hints.count, Int(MeshDefaults.carryHintDayWindow + 1) * 2)
    }

    func testExcludesThisDevicesOwnUserId() {
        let ownUserId = Data([1])
        let self_ = makeContact(userId: ownUserId, name: "Me (imported as a contact row)")
        let alice = makeContact(userId: Data([2]), name: "Alice")
        let now: Int64 = 1_700_000_000_000

        let hints = relayProxyHints(contacts: [self_, alice], ownUserId: ownUserId, nowMs: now)

        let ownHints = Set((0...MeshDefaults.carryHintDayWindow).map { daysAgo in
            computeRecipientHint(recipientUserId: ownUserId, timestampMs: now - daysAgo * MeshDefaults.msPerDay)
        })
        XCTAssertTrue(Set(hints).isDisjoint(with: ownHints))
        XCTAssertEqual(hints.count, Int(MeshDefaults.carryHintDayWindow + 1))
    }

    func testEmptyContactListProducesNoHints() {
        let hints = relayProxyHints(contacts: [], ownUserId: Data([1]), nowMs: 1_700_000_000_000)
        XCTAssertTrue(hints.isEmpty)
    }

    func testDifferentContactsProduceDisjointHintSets() {
        let ownUserId = Data([1])
        let alice = makeContact(userId: Data([2]), name: "Alice")
        let bob = makeContact(userId: Data([3]), name: "Bob")
        let now: Int64 = 1_700_000_000_000

        let aliceOnly = Set(relayProxyHints(contacts: [alice], ownUserId: ownUserId, nowMs: now))
        let bobOnly = Set(relayProxyHints(contacts: [bob], ownUserId: ownUserId, nowMs: now))

        XCTAssertTrue(aliceOnly.isDisjoint(with: bobOnly))
    }
}
