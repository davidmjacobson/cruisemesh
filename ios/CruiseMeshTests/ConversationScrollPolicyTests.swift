import XCTest
@testable import CruiseMesh

final class ConversationScrollPolicyTests: XCTestCase {
    func testInitialLoadScrollsToLatest() {
        XCTAssertEqual(
            ConversationScrollPolicy.decide(
                previousRowIds: [],
                currentRowIds: ["one", "two"],
                lateArrivalRowIds: [],
                isNearBottom: false,
                newestIsOwnMessage: false
            ),
            .autoScroll
        )
    }

    func testIncomingTailWhileReadingHistoryShowsChip() {
        XCTAssertEqual(
            ConversationScrollPolicy.decide(
                previousRowIds: ["one", "two"],
                currentRowIds: ["one", "two", "three"],
                lateArrivalRowIds: [],
                isNearBottom: false,
                newestIsOwnMessage: false
            ),
            .showNewMessages(targetRowId: nil)
        )
    }

    func testIncomingTailNearBottomAutoScrolls() {
        XCTAssertEqual(
            ConversationScrollPolicy.decide(
                previousRowIds: ["one", "two"],
                currentRowIds: ["one", "two", "three"],
                lateArrivalRowIds: [],
                isNearBottom: true,
                newestIsOwnMessage: false
            ),
            .autoScroll
        )
    }

    func testOwnSendAutoScrollsWhileReadingHistory() {
        XCTAssertEqual(
            ConversationScrollPolicy.decide(
                previousRowIds: ["one", "two"],
                currentRowIds: ["one", "two", "mine"],
                lateArrivalRowIds: [],
                isNearBottom: false,
                newestIsOwnMessage: true
            ),
            .autoScroll
        )
    }

    func testOrdinaryBackfillDoesNothing() {
        XCTAssertEqual(
            ConversationScrollPolicy.decide(
                previousRowIds: ["two", "three"],
                currentRowIds: ["one", "two", "three"],
                lateArrivalRowIds: [],
                isNearBottom: true,
                newestIsOwnMessage: false
            ),
            .none
        )
    }

    func testLateArrivalInsertedAboveTailGetsJumpTarget() {
        XCTAssertEqual(
            ConversationScrollPolicy.decide(
                previousRowIds: ["one", "three"],
                currentRowIds: ["one", "two", "three"],
                lateArrivalRowIds: ["two"],
                isNearBottom: true,
                newestIsOwnMessage: false
            ),
            .showNewMessages(targetRowId: "two")
        )
    }
}
