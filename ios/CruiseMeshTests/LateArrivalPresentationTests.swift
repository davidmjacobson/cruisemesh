import XCTest
@testable import CruiseMesh

/// The shell half of the late-arrival annotation: mapping the core's
/// per-row flags (`core/src/late_arrival.rs`) onto the row ids the chat views
/// render by. The displacement rule itself is pinned by the Rust tests.
final class LateArrivalPresentationTests: XCTestCase {
    private let ownId = Data([1])
    private let peerId = Data([2])
    private let chatId = Data([9])

    private let minute: Int64 = 60_000
    private var hour: Int64 { 60 * minute }

    private func message(sender: Data, lamport: UInt64, timestampMs: Int64) -> StoredMessage {
        StoredMessage(
            chatId: chatId,
            senderUserId: sender,
            lamport: lamport,
            timestamp: timestampMs,
            kind: ProtocolKind.text,
            payload: Data()
        )
    }

    private func arrival(sender: Data, lamport: UInt64, receivedAtMs: Int64) -> CoreMessageReceivedAt {
        CoreMessageReceivedAt(senderUserId: sender, lamport: lamport, receivedAtMs: receivedAtMs)
    }

    func testCarriedMessageAboveOwnReplyReportsItsArrival() {
        let messages = [
            message(sender: ownId, lamport: 1, timestampMs: 0),
            message(sender: peerId, lamport: 1, timestampMs: minute),
            message(sender: ownId, lamport: 2, timestampMs: 30 * minute)
        ]
        let received = [arrival(sender: peerId, lamport: 1, receivedAtMs: 3 * hour)]

        let flagged = lateArrivalTimesByKey(
            visibleMessages: messages,
            receivedTimes: received,
            ownUserId: ownId
        )

        XCTAssertEqual(flagged, [lateArrivalRowKey(messages[1]): 3 * hour])
    }

    func testThreadThatArrivedInOrderSaysNothing() {
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: 0),
            message(sender: peerId, lamport: 2, timestampMs: hour)
        ]
        let received = [
            arrival(sender: peerId, lamport: 1, receivedAtMs: 3 * hour),
            arrival(sender: peerId, lamport: 2, receivedAtMs: 4 * hour)
        ]

        XCTAssertTrue(
            lateArrivalTimesByKey(visibleMessages: messages, receivedTimes: received, ownUserId: ownId).isEmpty
        )
    }

    func testMessageWithoutRecordedArrivalIsNeverReported() {
        // Legacy rows predate arrival diagnostics: nothing to claim.
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: 0),
            message(sender: ownId, lamport: 1, timestampMs: 30 * minute)
        ]

        XCTAssertTrue(
            lateArrivalTimesByKey(visibleMessages: messages, receivedTimes: [], ownUserId: ownId).isEmpty
        )
    }

    func testArrivalRowsAreMatchedBySenderAsWellAsLamport() {
        // Two senders both at lamport 1: matching on lamport alone would hand
        // one sender's arrival time to the other's message.
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: 0),
            message(sender: ownId, lamport: 1, timestampMs: 30 * minute)
        ]
        let received = [
            arrival(sender: peerId, lamport: 1, receivedAtMs: 3 * hour),
            arrival(sender: Data([7]), lamport: 1, receivedAtMs: 9 * hour)
        ]

        XCTAssertEqual(
            lateArrivalTimesByKey(visibleMessages: messages, receivedTimes: received, ownUserId: ownId),
            [lateArrivalRowKey(messages[0]): 3 * hour]
        )
    }

    func testEmptyConversationIsHandled() {
        XCTAssertTrue(
            lateArrivalTimesByKey(visibleMessages: [], receivedTimes: [], ownUserId: ownId).isEmpty
        )
    }

    func testRowModelCarriesTheArrivalLabelOnlyForFlaggedRows() {
        let messages = [
            message(sender: ownId, lamport: 1, timestampMs: 0),
            message(sender: peerId, lamport: 1, timestampMs: minute),
            message(sender: ownId, lamport: 2, timestampMs: 30 * minute)
        ]
        let lateArrival = [lateArrivalRowKey(messages[1]): 3 * hour]

        let rows = ChatRowModel.build(from: messages, ownUserId: ownId, lateArrivalMs: lateArrival)

        XCTAssertNil(rows[0].arrivalLabel)
        XCTAssertNotNil(rows[1].arrivalLabel)
        XCTAssertTrue(rows[1].arrivalLabel?.hasPrefix("Arrived ") == true)
        XCTAssertNil(rows[2].arrivalLabel)
        // The bubble's own time still reports when it was sent, not received.
        XCTAssertNotEqual(rows[1].timeLabel, rows[1].arrivalLabel)
    }

    func testGroupRowModelCarriesTheArrivalLabel() {
        let messages = [
            message(sender: peerId, lamport: 1, timestampMs: 0),
            message(sender: ownId, lamport: 1, timestampMs: 30 * minute)
        ]
        let lateArrival = [lateArrivalRowKey(messages[0]): 3 * hour]

        let rows = GroupChatRowModel.build(
            from: messages,
            ownUserId: ownId,
            senderLabel: { _ in "Ana" },
            lateArrivalMs: lateArrival
        )

        XCTAssertNotNil(rows[0].arrivalLabel)
        XCTAssertNil(rows[1].arrivalLabel)
    }
}
