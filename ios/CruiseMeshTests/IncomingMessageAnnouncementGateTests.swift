import XCTest
@testable import CruiseMesh

final class IncomingMessageAnnouncementGateTests: XCTestCase {
    func testDirectVisibleKindAnnouncesOnlyWhenChatIsOffScreen() {
        let probe = AnnouncementProbe()
        let gate = IncomingMessageAnnouncementGate(announcer: probe)
        let contact = makeContact(name: "Alice")

        gate.announceDirectIfNeeded(
            chatVisible: true,
            kind: ProtocolKind.text,
            contact: contact,
            preview: { "on screen" }
        )
        gate.announceDirectIfNeeded(
            chatVisible: false,
            kind: ProtocolKind.text,
            contact: contact,
            preview: { "off screen" }
        )

        XCTAssertEqual(probe.directPreviews, ["off screen"])
    }

    func testHiddenKindsNeverAnnounce() {
        let probe = AnnouncementProbe()
        let gate = IncomingMessageAnnouncementGate(announcer: probe)

        gate.announceDirectIfNeeded(
            chatVisible: false,
            kind: ProtocolKind.receipt,
            contact: makeContact(name: "Alice"),
            preview: { "hidden" }
        )

        XCTAssertTrue(probe.directPreviews.isEmpty)
    }

    func testGroupMessageUsesGroupAnnouncementAndLazilyBuildsPreview() throws {
        let probe = AnnouncementProbe()
        let gate = IncomingMessageAnnouncementGate(announcer: probe)
        let member = generateIdentity()
        let group = try createGroup(name: "Family", memberUserIds: [member.userId])
        var previewBuilds = 0

        gate.announceGroupIfNeeded(
            chatVisible: true,
            kind: ProtocolKind.text,
            group: group,
            senderName: { "Alice" },
            preview: { previewBuilds += 1; return "ignored" }
        )
        gate.announceGroupIfNeeded(
            chatVisible: false,
            kind: ProtocolKind.text,
            group: group,
            senderName: { "Alice" },
            preview: { previewBuilds += 1; return "hello" }
        )

        XCTAssertEqual(previewBuilds, 1)
        XCTAssertEqual(probe.groupMessages, ["Alice: hello"])
    }

    func testGroupInviteHasDedicatedAnnouncement() throws {
        let probe = AnnouncementProbe()
        let gate = IncomingMessageAnnouncementGate(announcer: probe)
        let member = generateIdentity()
        let group = try createGroup(name: "Family", memberUserIds: [member.userId])

        gate.announceGroupInviteIfNeeded(chatVisible: true, group: group)
        gate.announceGroupInviteIfNeeded(chatVisible: false, group: group)

        XCTAssertEqual(probe.groupInvites, ["Family"])
        XCTAssertTrue(probe.groupMessages.isEmpty)
    }

    private func makeContact(name: String) -> Contact {
        Contact(
            userId: Data(repeating: 1, count: 16),
            name: name,
            signPk: Data(repeating: 2, count: 32),
            agreePk: Data(repeating: 3, count: 32),
            relayUrl: nil,
            relayToken: nil,
            nickname: nil
        )
    }
}

private final class AnnouncementProbe: IncomingMessageAnnouncing {
    var directPreviews: [String] = []
    var groupMessages: [String] = []
    var groupInvites: [String] = []

    func announceDirectMessage(contact: Contact, preview: String) {
        directPreviews.append(preview)
    }

    func announceGroupMessage(group: Group, senderName: String, preview: String) {
        groupMessages.append("\(senderName): \(preview)")
    }

    func announceGroupInvite(group: Group) {
        groupInvites.append(group.name)
    }

    func announceFriendAdded(contact: Contact) {}
    func announceSharedRequest(name: String, userId: Data) {}
}
