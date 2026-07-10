import XCTest
@testable import CruiseMesh

final class GroupMessagingTests: XCTestCase {
    func testGroupEnvelopeSealsOnceForEveryMember() throws {
        let alice = generateIdentity()
        let bob = generateIdentity()
        let group = try createGroup(
            name: "Family",
            memberUserIds: [alice.userId, bob.userId]
        )
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: alice.userId,
            lamport: 1,
            timestamp: 1_700_000_000_000,
            kind: ProtocolKind.text,
            payload: Data("hello group".utf8)
        )

        let envelope = try XCTUnwrap(buildOutboundGroupEnvelope(
            identity: alice,
            group: group,
            message: message
        ))
        XCTAssertEqual(envelope.recipientUserId, group.id)
        XCTAssertEqual(envelope.recipientHint, computeRecipientHint(
            recipientUserId: group.id,
            timestampMs: message.timestamp
        ))

        let opened = try openGroupMessage(group: group, sealed: envelope.sealed)
        let body = try decodeMessageBody(bytes: opened.payload)
        XCTAssertEqual(opened.senderUserId, alice.userId)
        XCTAssertEqual(body.chatId, group.id)
        XCTAssertEqual(body.content, Data("hello group".utf8))
    }

    func testGroupEnvelopeRejectsNonMemberSender() throws {
        let alice = generateIdentity()
        let outsider = generateIdentity()
        let group = try createGroup(name: "Family", memberUserIds: [alice.userId])
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: outsider.userId,
            lamport: 1,
            timestamp: 1,
            kind: ProtocolKind.text,
            payload: Data("nope".utf8)
        )

        XCTAssertNil(buildOutboundGroupEnvelope(identity: outsider, group: group, message: message))
    }

    func testCreateAndInvitePersistsGroupAndPairwiseInvite() throws {
        let store = try MessageStore.open(path: ":memory:")
        let alice = generateIdentity()
        let bob = generateIdentity()
        let bobContact = Contact(
            userId: bob.userId,
            name: "Bob",
            signPk: bob.signPk,
            agreePk: bob.agreePk,
            relayUrl: nil,
            relayToken: nil
        )
        try store.upsertContact(contact: bobContact)

        let sender = GroupSender(store: store, identity: alice)
        let group = try XCTUnwrap(sender.createAndInvite(name: "Family", members: [bobContact]))
        XCTAssertEqual(try store.listGroups(), [group])

        let messages = try store.messagesForChat(chatId: group.id)
        XCTAssertEqual(messages.count, 1)
        XCTAssertEqual(messages[0].kind, ProtocolKind.groupInvite)

        let envelopes = try store.outboundEnvelopesAfter(
            chatId: group.id,
            senderUserId: alice.userId,
            afterLamport: 0
        )
        XCTAssertEqual(envelopes.count, 1)
        XCTAssertEqual(envelopes[0].recipientUserId, bob.userId)
        let opened = try openMessage(recipient: bob, sealed: envelopes[0].sealed)
        let body = try decodeMessageBody(bytes: opened.payload)
        XCTAssertEqual(body.kind, ProtocolKind.groupInvite)
        XCTAssertEqual(try decodeGroupInviteContent(bytes: body.content), group)
    }

    func testSendTextPersistsGroupEnvelope() throws {
        let store = try MessageStore.open(path: ":memory:")
        let alice = generateIdentity()
        let bob = generateIdentity()
        let group = try createGroup(name: "Family", memberUserIds: [alice.userId, bob.userId])
        try store.upsertGroup(group: group)

        GroupSender(store: store, identity: alice).sendText(group: group, text: "hello")

        let messages = try store.messagesForChat(chatId: group.id)
        XCTAssertEqual(messages.count, 1)
        XCTAssertEqual(messages[0].payload, Data("hello".utf8))
        let envelopes = try store.outboundEnvelopesAfter(
            chatId: group.id,
            senderUserId: alice.userId,
            afterLamport: 0
        )
        XCTAssertEqual(envelopes.count, 1)
        XCTAssertEqual(envelopes[0].recipientUserId, group.id)
    }
}
