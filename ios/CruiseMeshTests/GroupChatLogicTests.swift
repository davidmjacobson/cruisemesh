import XCTest
@testable import CruiseMesh

final class GroupChatLogicTests: XCTestCase {
    private func message(chatId: Data, sender: Data, lamport: UInt64, timestamp: Int64, kind: UInt8, payload: Data = Data()) -> StoredMessage {
        StoredMessage(chatId: chatId, senderUserId: sender, lamport: lamport, timestamp: timestamp, kind: kind, payload: payload)
    }

    func testGroupInviteIsVisibleWithSystemPreview() {
        let groupId = Data(repeating: 0x11, count: 16)
        let msg = message(chatId: groupId, sender: Data([1]), lamport: 1, timestamp: 1000, kind: ProtocolKind.groupInvite)
        XCTAssertEqual(ChatListLogic.lastVisibleMessage([msg])?.lamport, msg.lamport)
        XCTAssertEqual(ChatListLogic.previewText(msg, groupName: "Bridge"), "Group created: Bridge")
        XCTAssertEqual(ChatListLogic.previewText(msg), "Group invite")
    }

    func testComputeGroupUnreadCountsPerSenderStreams() {
        let ownId = Data([1])
        let alice = Data([2])
        let bob = Data([3])
        let groupId = Data(repeating: 0x22, count: 16)
        let messages = [
            message(chatId: groupId, sender: alice, lamport: 1, timestamp: 1000, kind: ProtocolKind.text, payload: Data("a1".utf8)),
            message(chatId: groupId, sender: alice, lamport: 2, timestamp: 2000, kind: ProtocolKind.text, payload: Data("a2".utf8)),
            message(chatId: groupId, sender: bob, lamport: 1, timestamp: 3000, kind: ProtocolKind.text, payload: Data("b1".utf8)),
            message(chatId: groupId, sender: ownId, lamport: 1, timestamp: 4000, kind: ProtocolKind.text, payload: Data("me".utf8)),
        ]
        let readThrough: [Data: UInt64] = [alice: 1, bob: 0]
        let unread = ChatListLogic.computeGroupUnread(messages: messages, ownUserId: ownId) { sender in
            readThrough[sender] ?? 0
        }
        // alice's lamport 2 + bob's lamport 1
        XCTAssertEqual(unread, 2)
    }
}
