import XCTest
@testable import CruiseMesh

final class ReplyMessagingTests: XCTestCase {
    override func tearDown() {
        MeshRouter.reset()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        super.tearDown()
    }

    func testPairwiseReplyIsEncryptedStoredAndResolved() throws {
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
        let sender = RealMeshSender(store: store, identity: alice)

        sender.sendText(contact: bobContact, text: "original")
        let originalEnvelope = try XCTUnwrap(try store.outboundEnvelopesAfter(
            chatId: bob.userId,
            senderUserId: alice.userId,
            afterLamport: 0
        ).first)

        sender.sendText(
            contact: bobContact,
            text: "reply",
            replyToMsgId: originalEnvelope.msgId
        )

        let messages = try store.messagesForChat(chatId: bob.userId)
        let reply = try XCTUnwrap(messages.first { $0.lamport == 2 })
        let reference = try XCTUnwrap(try store.messageReference(
            chatId: reply.chatId,
            senderUserId: reply.senderUserId,
            lamport: reply.lamport
        ))
        XCTAssertEqual(reference.replyToMsgId, originalEnvelope.msgId)

        let replyEnvelope = try XCTUnwrap(try store.outboundEnvelopesAfter(
            chatId: bob.userId,
            senderUserId: alice.userId,
            afterLamport: 1
        ).first)
        let opened = try openMessage(recipient: bob, sealed: replyEnvelope.sealed)
        let decoded = try decodeExtendedMessageBody(bytes: opened.payload)
        XCTAssertEqual(decoded.replyToMsgId, originalEnvelope.msgId)

        let metadata = loadMessageReplyMetadata(store: store, messages: messages) { _ in "You" }
        let quoted = try XCTUnwrap(metadata[replyMessageKey(reply)]?.quoted)
        XCTAssertEqual(quoted.senderLabel, "You")
        XCTAssertEqual(quoted.text, "original")
        XCTAssertEqual(quoted.target?.lamport, 1)
    }

    func testGroupReplyRoundTripsInsideSharedCiphertext() throws {
        let alice = generateIdentity()
        let bob = generateIdentity()
        let group = try createGroup(name: "Family", memberUserIds: [alice.userId, bob.userId])
        let replyToMsgId = Data((1...16).map { UInt8($0) })
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: alice.userId,
            lamport: 2,
            timestamp: 1_700_000_001_000,
            kind: ProtocolKind.text,
            payload: Data("sounds good".utf8)
        )

        let envelope = try XCTUnwrap(buildOutboundGroupEnvelope(
            identity: alice,
            group: group,
            message: message,
            replyToMsgId: replyToMsgId
        ))
        let opened = try openGroupMessage(group: group, sealed: envelope.sealed)
        let decoded = try decodeExtendedMessageBody(bytes: opened.payload)

        XCTAssertEqual(decoded.content, Data("sounds good".utf8))
        XCTAssertEqual(decoded.replyToMsgId, replyToMsgId)
    }

    func testMissingAndPhotoQuotePresentation() {
        let missing = quotedMessagePreview(target: nil) { _ in "unused" }
        XCTAssertEqual(missing.text, "Original message unavailable")
        XCTAssertNil(missing.senderLabel)
        XCTAssertNil(missing.target)

        let photo = AttachmentPayload(
            mediaType: .image,
            mimeType: "image/jpeg",
            durationMs: 0,
            blob: Data([1, 2, 3]),
            caption: "pool deck"
        )
        let message = StoredMessage(
            chatId: Data([1]),
            senderUserId: Data([2]),
            lamport: 1,
            timestamp: 1,
            kind: ProtocolKind.attachmentManifest,
            payload: photo.encode()
        )
        XCTAssertEqual(quotedMessageText(message), "📷 pool deck")
    }
}
