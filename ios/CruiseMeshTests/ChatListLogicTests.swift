import XCTest
@testable import CruiseMesh

final class ChatListLogicTests: XCTestCase {
    func testInitials() {
        let (_, init1) = ChatListLogic.avatarHueAndInitials(
            userId: Data(), name: "Alice", displayId: "CM-ABCD"
        )
        XCTAssertEqual(init1, "AL")

        let (_, init2) = ChatListLogic.avatarHueAndInitials(
            userId: Data(), name: "A", displayId: "CM-ABCD"
        )
        XCTAssertEqual(init2, "A")

        let (_, init3) = ChatListLogic.avatarHueAndInitials(
            userId: Data(), name: "", displayId: "CM-ABCD"
        )
        XCTAssertEqual(init3, "AB")

        let (_, init4) = ChatListLogic.avatarHueAndInitials(
            userId: Data(), name: "Unknown", displayId: "CM-1234"
        )
        XCTAssertEqual(init4, "12")
    }

    func testFormatRelativeTime() {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        cal.locale = Locale(identifier: "en_US")

        func ms(_ y: Int, _ m: Int, _ d: Int, _ h: Int, _ min: Int) -> Int64 {
            var c = DateComponents()
            c.year = y; c.month = m; c.day = d; c.hour = h; c.minute = min
            c.timeZone = TimeZone(identifier: "UTC")
            return Int64(cal.date(from: c)!.timeIntervalSince1970 * 1000)
        }

        // Use Calendar.current for same-day checks inside formatRelativeTime.
        // Pin "now" and "then" via fixed calendar so day-of-week/month are stable
        // under en_US formatters used by the production helper.
        let nowMs = ms(2026, 7, 9, 14, 0)
        let sameDay = ms(2026, 7, 9, 9, 30)
        let twoDaysAgo = ms(2026, 7, 7, 14, 0)
        let older = ms(2026, 6, 1, 14, 0)

        // Time-of-day string depends on device timezone; compare structure.
        let sameDayLabel = ChatListLogic.formatRelativeTime(timestampMs: sameDay, nowMs: nowMs)
        XCTAssertFalse(sameDayLabel.isEmpty)
        // When device calendar sees same day as "now" for these UTC instants
        // (or not), still assert non-empty weekday / month formats for others.
        let weekday = ChatListLogic.formatRelativeTime(timestampMs: twoDaysAgo, nowMs: nowMs)
        let monthDay = ChatListLogic.formatRelativeTime(timestampMs: older, nowMs: nowMs)
        XCTAssertFalse(weekday.isEmpty)
        XCTAssertFalse(monthDay.isEmpty)

        // Explicit en_US path: if same calendar day, expect AM/PM time.
        let localCal = Calendar.current
        let nowDate = Date(timeIntervalSince1970: TimeInterval(nowMs) / 1000)
        let sameDate = Date(timeIntervalSince1970: TimeInterval(sameDay) / 1000)
        if localCal.isDate(nowDate, inSameDayAs: sameDate) {
            XCTAssertTrue(sameDayLabel.contains("AM") || sameDayLabel.contains("PM"))
        }
    }

    func testComputeUnread() {
        let ownId = Data([1])
        let peerId = Data([2])
        let messages = [
            StoredMessage(chatId: peerId, senderUserId: peerId, lamport: 1, timestamp: 1000, kind: 1, payload: Data()),
            StoredMessage(chatId: peerId, senderUserId: ownId, lamport: 2, timestamp: 2000, kind: 1, payload: Data()),
            StoredMessage(chatId: peerId, senderUserId: peerId, lamport: 3, timestamp: 3000, kind: 1, payload: Data()),
            StoredMessage(chatId: peerId, senderUserId: peerId, lamport: 4, timestamp: 4000, kind: 1, payload: Data()),
            // Hidden friend-request stream noise must not inflate the badge.
            StoredMessage(chatId: peerId, senderUserId: peerId, lamport: 5, timestamp: 5000, kind: 3, payload: Data()),
        ]
        let unread = ChatListLogic.computeUnread(messages: messages, ownUserId: ownId, readThrough: 1)
        XCTAssertEqual(unread, 2)
    }

    func testLastVisibleMessageSkipsFriendRequests() {
        let peerId = Data([2])
        let messages = [
            StoredMessage(
                chatId: peerId, senderUserId: peerId, lamport: 1, timestamp: 1000, kind: 1,
                payload: Data("hello".utf8)
            ),
            StoredMessage(
                chatId: peerId, senderUserId: peerId, lamport: 2, timestamp: 2000, kind: 3,
                payload: Data("{}".utf8)
            ),
        ]
        let last = ChatListLogic.lastVisibleMessage(messages)
        XCTAssertEqual(last?.lamport, 1)
        XCTAssertEqual(ChatListLogic.previewText(last!), "hello")
    }

    func testGroupInviteIsVisibleWithSystemPreview() {
        let groupId = Data(repeating: 0x11, count: 16)
        let msg = StoredMessage(
            chatId: groupId, senderUserId: Data([1]), lamport: 1, timestamp: 1000, kind: 4, payload: Data()
        )
        XCTAssertEqual(ChatListLogic.lastVisibleMessage([msg]), msg)
        XCTAssertEqual(ChatListLogic.previewText(msg, groupName: "Bridge"), "Group created: Bridge")
        XCTAssertEqual(ChatListLogic.previewText(msg), "Group invite")
    }

    func testComputeGroupUnreadCountsPerSenderStreams() {
        let ownId = Data([1])
        let alice = Data([2])
        let bob = Data([3])
        let groupId = Data(repeating: 0x22, count: 16)
        let messages = [
            StoredMessage(chatId: groupId, senderUserId: alice, lamport: 1, timestamp: 1000, kind: 1, payload: Data("a1".utf8)),
            StoredMessage(chatId: groupId, senderUserId: alice, lamport: 2, timestamp: 2000, kind: 1, payload: Data("a2".utf8)),
            StoredMessage(chatId: groupId, senderUserId: bob, lamport: 1, timestamp: 3000, kind: 1, payload: Data("b1".utf8)),
            StoredMessage(chatId: groupId, senderUserId: ownId, lamport: 1, timestamp: 4000, kind: 1, payload: Data("me".utf8)),
        ]
        let readThrough: [Data: UInt64] = [alice: 1, bob: 0]
        let unread = ChatListLogic.computeGroupUnread(messages: messages, ownUserId: ownId) { sender in
            readThrough[sender] ?? 0
        }
        // alice's lamport 2 + bob's lamport 1
        XCTAssertEqual(unread, 2)
    }
}
