import XCTest
@testable import CruiseMesh

final class ChatReadMarkerTests: XCTestCase {
    func testDirectChatMarksHighestPeerLamportRead() throws {
        let ownId = Data(repeating: 1, count: 16)
        let peerId = Data(repeating: 2, count: 16)
        let store = try MessageStore.open(path: ":memory:")
        try store.insertMessage(message: StoredMessage(
            chatId: peerId,
            senderUserId: peerId,
            lamport: 7,
            timestamp: 1,
            kind: ProtocolKind.text,
            payload: Data("hello".utf8)
        ))

        XCTAssertEqual(
            ChatReadMarker.markRead(
                store: store,
                ownUserId: ownId,
                chatId: peerId,
                isGroup: false
            ),
            1
        )
        XCTAssertEqual(
            try store.outgoingReceiptThrough(
                chatId: peerId,
                senderUserId: peerId,
                receiptType: ReceiptType.read
            ),
            7
        )
    }

    func testGroupMarksEachOtherMemberStreamRead() throws {
        let own = generateIdentity()
        let alice = generateIdentity()
        let bob = generateIdentity()
        let group = try createGroup(
            name: "Family",
            memberUserIds: [own.userId, alice.userId, bob.userId]
        )
        let store = try MessageStore.open(path: ":memory:")
        try store.upsertGroup(group: group)
        for (sender, lamport) in [(alice.userId, UInt64(3)), (bob.userId, UInt64(5))] {
            try store.insertMessage(message: StoredMessage(
                chatId: group.id,
                senderUserId: sender,
                lamport: lamport,
                timestamp: Int64(lamport),
                kind: ProtocolKind.text,
                payload: Data("hello".utf8)
            ))
        }

        XCTAssertEqual(
            ChatReadMarker.markRead(
                store: store,
                ownUserId: own.userId,
                chatId: group.id,
                isGroup: true
            ),
            2
        )
        XCTAssertEqual(
            try store.outgoingReceiptThrough(
                chatId: group.id,
                senderUserId: alice.userId,
                receiptType: ReceiptType.read
            ),
            3
        )
        XCTAssertEqual(
            try store.outgoingReceiptThrough(
                chatId: group.id,
                senderUserId: bob.userId,
                receiptType: ReceiptType.read
            ),
            5
        )
    }
}
