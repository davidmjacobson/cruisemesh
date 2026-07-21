import XCTest
@testable import CruiseMesh

/// The Message-info bug fix: `messageInfoRows` returns typed rows
/// (label+value or a plain sentence) instead of one big string that
/// `MessageInfoSheet` used to re-parse by splitting each line on its first
/// `:` -- which corrupted any sentence containing a colon of its own, most
/// notably the arrival line's "5:14 PM" time (rendered as "…· 5" / "14 PM").
/// These tests exercise the row builder directly, no SwiftUI involved.
final class MessageInfoRowTests: XCTestCase {
    private func message(
        timestampMs: Int64,
        lamport: UInt64 = 1,
        kind: UInt8 = ProtocolKind.text
    ) -> StoredMessage {
        StoredMessage(
            chatId: Data(repeating: 1, count: 16),
            senderUserId: Data(repeating: 2, count: 16),
            lamport: lamport,
            timestamp: timestampMs,
            kind: kind,
            payload: Data("hello".utf8)
        )
    }

    /// The bug's exact reproduction: a received message's arrival line
    /// contains a "h:mm a" time, which always contains a colon. The old
    /// text-splitting renderer would mis-split this into a bogus
    /// LabeledContent pair; the row builder must keep it as one sentence.
    func testArrivalLineWithColonInTimeStaysOneSentence() {
        let timestamp: Int64 = 1_700_000_000_000
        let arrival = MessageArrival(transport: 1, hopsTaken: 2, receivedAt: timestamp)
        let rows = messageInfoRows(
            message: message(timestampMs: timestamp),
            isOwn: false,
            tick: nil,
            arrival: arrival
        )

        guard case .sentence(let text)? = rows.last else {
            return XCTFail("expected the arrival line to be a plain sentence, got \(String(describing: rows.last))")
        }
        XCTAssertTrue(text.hasPrefix("Arrived via another device over BLE · ~2 hops · "))
        // The whole "h:mm a" time (with its colon) survives intact inside
        // the one row -- nothing downstream ever splits on ":".
        XCTAssertTrue(text.contains(":"), "arrival time should still contain its colon")
        XCTAssertFalse(rows.contains { row in
            if case .labeled(let label, _) = row { return label == "Arrived via another device over BLE · ~2 hops · 5" }
            return false
        })
    }

    func testReceivedMessageRowsAreDirectionThenTimeThenArrival() {
        let timestamp: Int64 = 1_700_000_000_000
        let arrival = MessageArrival(transport: 0, hopsTaken: 0, receivedAt: timestamp)
        let rows = messageInfoRows(message: message(timestampMs: timestamp), isOwn: false, tick: nil, arrival: arrival)

        XCTAssertEqual(rows.count, 3)
        XCTAssertEqual(rows[0], .sentence("Received"))
        guard case .labeled(let label, _) = rows[1] else {
            return XCTFail("expected a labeled Time row")
        }
        XCTAssertEqual(label, "Time")
        guard case .sentence(let arrivalText) = rows[2] else {
            return XCTFail("expected a plain arrival sentence")
        }
        XCTAssertTrue(arrivalText.hasPrefix("Arrived via direct BLE · ~0 hops · "))
    }

    func testReceivedMessageWithNoArrivalDataOmitsArrivalRow() {
        let timestamp: Int64 = 1_700_000_000_000
        let rows = messageInfoRows(message: message(timestampMs: timestamp), isOwn: false, tick: nil)
        XCTAssertEqual(rows.count, 2)
        XCTAssertEqual(rows[0], .sentence("Received"))
    }

    func testOwnDeliveredMessageShowsStatusAndConfirmedRoute() {
        let timestamp: Int64 = 1_700_000_000_000
        let rows = messageInfoRows(
            message: message(timestampMs: timestamp),
            isOwn: true,
            tick: .delivered,
            deliveredViaRoute: "relay"
        )
        XCTAssertEqual(rows[0], .sentence("Sent by you"))
        XCTAssertEqual(rows[2], .labeled(label: "Status", value: tickLegendText(.delivered)))
        XCTAssertEqual(rows[3], .sentence("Delivery confirmed via relay"))
    }

    func testOwnSentMessageStillTryingShowsCountdownStatus() {
        let timestamp: Int64 = 1_700_000_000_000
        let nowMs = timestamp + 5 * 60_000
        let expiry = nowMs + 60 * 60_000 // 1 hour from now
        let rows = messageInfoRows(
            message: message(timestampMs: timestamp),
            isOwn: true,
            tick: .sent,
            outboundExpiryMs: expiry,
            nowMs: nowMs
        )
        XCTAssertEqual(rows.count, 3)
        XCTAssertEqual(rows[2], .labeled(label: "Status", value: "Still trying — expires in 1 hour"))
    }

    func testOwnSentMessageExpiredShowsNotDeliveredStatus() {
        let timestamp: Int64 = 1_700_000_000_000
        let nowMs = timestamp + 60 * 60_000
        let expiry = timestamp + 30 * 60_000 // already in the past relative to nowMs
        let rows = messageInfoRows(
            message: message(timestampMs: timestamp),
            isOwn: true,
            tick: .sent,
            outboundExpiryMs: expiry,
            nowMs: nowMs
        )
        XCTAssertEqual(rows[2], .labeled(label: "Status", value: "Not delivered — expired"))
    }

    func testOwnMessageWithoutDeliveryConfirmationOmitsArrivalSentence() {
        let timestamp: Int64 = 1_700_000_000_000
        let rows = messageInfoRows(
            message: message(timestampMs: timestamp),
            isOwn: true,
            tick: .delivered,
            deliveredViaRoute: nil
        )
        // "Sent by you", "Time", "Status" -- no fourth row since there's no
        // confirmed route to report.
        XCTAssertEqual(rows.count, 3)
    }
}
