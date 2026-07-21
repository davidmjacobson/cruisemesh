import XCTest
@testable import CruiseMesh

/// FI8: `ChatRowModel.build`/`GroupChatRowModel.build` replace per-row
/// recomputation (day breaks, message grouping, sender-label runs, reaction
/// summaries) that made the chat thread bodies O(n^2) in message count.
/// These tests exercise the row builders directly — no SwiftUI involved.
final class ChatRowModelTests: XCTestCase {
    private let ownId = Data([1])
    private let peerId = Data([2])
    private let chatId = Data([9])

    private func message(
        sender: Data,
        lamport: UInt64,
        timestampMs: Int64,
        kind: UInt8 = ProtocolKind.text,
        payload: Data = Data()
    ) -> StoredMessage {
        StoredMessage(chatId: chatId, senderUserId: sender, lamport: lamport, timestamp: timestampMs, kind: kind, payload: payload)
    }

    private func reactionMessage(on target: StoredMessage, emoji: String, sender: Data, lamport: UInt64, timestampMs: Int64) -> StoredMessage {
        let payload = ReactionPayload(
            target: MessageTarget(senderUserId: target.senderUserId, lamport: target.lamport, kind: target.kind),
            emoji: emoji
        ).encode()!
        return message(sender: sender, lamport: lamport, timestampMs: timestampMs, kind: ProtocolKind.reaction, payload: payload)
    }

    // Anchors expressed in terms of `Calendar.current`/`startOfDay`, matching
    // exactly what the production `cal.isDate(_:inSameDayAs:)` calls use, so
    // "same day"/"different day" assertions hold under any CI time zone.
    private func startOfTodayMs() -> Int64 {
        Int64(Calendar.current.startOfDay(for: Date()).timeIntervalSince1970 * 1000)
    }

    private func expectedTimeLabel(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateFormat = "h:mm a"
        f.locale = .current
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    private func expectedDayLabel(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateFormat = "MMMM d, yyyy"
        f.locale = .current
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    // MARK: - ChatRowModel (1:1 chat)

    func testFirstRowAlwaysShowsDayBreak() {
        let ts = startOfTodayMs() + 3_600_000
        let rows = ChatRowModel.build(from: [message(sender: peerId, lamport: 1, timestampMs: ts)], ownUserId: ownId)
        XCTAssertEqual(rows.count, 1)
        XCTAssertTrue(rows[0].showDayBreak)
        XCTAssertEqual(rows[0].dayLabel, expectedDayLabel(ts))
        XCTAssertEqual(rows[0].timeLabel, expectedTimeLabel(ts))
    }

    func testNoDayBreakWithinSameDayButBreaksOnNextDay() {
        let today = startOfTodayMs() + 3_600_000
        let sameDayLater = today + 2 * 60_000
        let nextDay = startOfTodayMs() + 25 * 3_600_000 // > 24h past start-of-today
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: today),
            message(sender: peerId, lamport: 2, timestampMs: sameDayLater),
            message(sender: peerId, lamport: 3, timestampMs: nextDay),
        ]
        let rows = ChatRowModel.build(from: messages, ownUserId: ownId)
        XCTAssertEqual(rows.count, 3)
        XCTAssertTrue(rows[0].showDayBreak)
        XCTAssertFalse(rows[1].showDayBreak)
        XCTAssertEqual(rows[1].dayLabel, "")
        XCTAssertTrue(rows[2].showDayBreak)
        XCTAssertEqual(rows[2].dayLabel, expectedDayLabel(nextDay))
    }

    func testGroupingJoinsSameSenderWithinFiveMinutes() {
        let base = startOfTodayMs() + 3_600_000
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: base),
            message(sender: peerId, lamport: 2, timestampMs: base + 4 * 60_000),
        ]
        let rows = ChatRowModel.build(from: messages, ownUserId: ownId)
        XCTAssertFalse(rows[0].grouping.joinsPrevious)
        XCTAssertTrue(rows[0].grouping.joinsNext)
        XCTAssertTrue(rows[1].grouping.joinsPrevious)
        XCTAssertFalse(rows[1].grouping.joinsNext)
        // Grouped continuation row hides its own timestamp.
        XCTAssertFalse(rows[0].grouping.showTimestamp)
        XCTAssertTrue(rows[1].grouping.showTimestamp)
    }

    func testGroupingBreaksOnSenderChangeGapOrDayBoundary() {
        let base = startOfTodayMs() + 3_600_000
        // Different sender: never joins.
        let differentSender = [
            message(sender: peerId, lamport: 1, timestampMs: base),
            message(sender: ownId, lamport: 1, timestampMs: base + 60_000),
        ]
        let rowsA = ChatRowModel.build(from: differentSender, ownUserId: ownId)
        XCTAssertFalse(rowsA[1].grouping.joinsPrevious)

        // Same sender, > 5 minutes apart: does not join.
        let bigGap = [
            message(sender: peerId, lamport: 1, timestampMs: base),
            message(sender: peerId, lamport: 2, timestampMs: base + 6 * 60_000),
        ]
        let rowsB = ChatRowModel.build(from: bigGap, ownUserId: ownId)
        XCTAssertFalse(rowsB[1].grouping.joinsPrevious)

        // Same sender, close together, but across the day boundary: does not join.
        let acrossDay = [
            message(sender: peerId, lamport: 1, timestampMs: startOfTodayMs() - 60_000),
            message(sender: peerId, lamport: 2, timestampMs: startOfTodayMs() + 60_000),
        ]
        let rowsC = ChatRowModel.build(from: acrossDay, ownUserId: ownId)
        XCTAssertFalse(rowsC[1].grouping.joinsPrevious)
        XCTAssertTrue(rowsC[1].showDayBreak)
    }

    func testHiddenKindsAreFilteredOut() {
        let base = startOfTodayMs() + 3_600_000
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: base, kind: ProtocolKind.text),
            // Friend-request stream noise must not become a visible row.
            message(sender: peerId, lamport: 2, timestampMs: base + 60_000, kind: ProtocolKind.friendRequest),
        ]
        let rows = ChatRowModel.build(from: messages, ownUserId: ownId)
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].message.lamport, 1)
    }

    func testReactionsAttachToTheCorrectRowOnly() {
        let base = startOfTodayMs() + 3_600_000
        let target = message(sender: peerId, lamport: 1, timestampMs: base)
        let untouched = message(sender: peerId, lamport: 2, timestampMs: base + 60_000)
        let reaction = reactionMessage(on: target, emoji: "👍", sender: ownId, lamport: 1, timestampMs: base + 30_000)
        let rows = ChatRowModel.build(from: [target, untouched, reaction], ownUserId: ownId)
        let targetRow = rows.first { $0.message.lamport == 1 }
        let untouchedRow = rows.first { $0.message.lamport == 2 }
        XCTAssertEqual(targetRow?.reactions.map(\.emoji), ["👍"])
        XCTAssertEqual(untouchedRow?.reactions ?? [], [])
    }

    // MARK: - GroupChatRowModel (group chat)

    func testGroupSenderLabelOnlyOnNewRun() {
        let base = startOfTodayMs() + 3_600_000
        let alice = Data([10])
        let messages = [
            message(sender: alice, lamport: 1, timestampMs: base),
            message(sender: alice, lamport: 2, timestampMs: base + 60_000),
            message(sender: ownId, lamport: 1, timestampMs: base + 2 * 60_000),
            message(sender: alice, lamport: 3, timestampMs: base + 3 * 60_000),
        ]
        let rows = GroupChatRowModel.build(from: messages, ownUserId: ownId) { sender in
            sender == alice ? "Alice" : "You"
        }
        XCTAssertEqual(rows.count, 4)
        XCTAssertEqual(rows[0].senderLabel, "Alice") // first message of alice's run
        XCTAssertNil(rows[1].senderLabel) // continues alice's run
        XCTAssertNil(rows[2].senderLabel) // own message never gets a label
        XCTAssertEqual(rows[3].senderLabel, "Alice") // new run after own message interrupts
    }

    func testGroupRowTimeLabelAndReactions() {
        let base = startOfTodayMs() + 3_600_000
        let alice = Data([10])
        let target = message(sender: alice, lamport: 1, timestampMs: base)
        let reaction = reactionMessage(on: target, emoji: "❤️", sender: ownId, lamport: 1, timestampMs: base + 60_000)
        let rows = GroupChatRowModel.build(from: [target, reaction], ownUserId: ownId) { _ in "Alice" }
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].timeLabel, expectedTimeLabel(base))
        XCTAssertEqual(rows[0].reactions.map(\.emoji), ["❤️"])
    }

    func testGroupHiddenKindsAreFilteredOut() {
        let base = startOfTodayMs() + 3_600_000
        let alice = Data([10])
        let messages = [
            message(sender: alice, lamport: 1, timestampMs: base, kind: ProtocolKind.text),
            message(sender: alice, lamport: 2, timestampMs: base + 60_000, kind: ProtocolKind.friendRequest),
        ]
        let rows = GroupChatRowModel.build(from: messages, ownUserId: ownId) { _ in "Alice" }
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].message.lamport, 1)
    }
}
