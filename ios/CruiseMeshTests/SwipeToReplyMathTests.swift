import XCTest
@testable import CruiseMesh

final class SwipeToReplyMathTests: XCTestCase {
    func testLeftwardDragIsIgnored() {
        XCTAssertEqual(SwipeToReplyMath.clampOffset(-40, maxDrag: 80), 0)
        XCTAssertEqual(SwipeToReplyMath.clampOffset(0, maxDrag: 80), 0)
    }

    func testWithinMaxDragTracksTheFinger() {
        XCTAssertEqual(SwipeToReplyMath.clampOffset(50, maxDrag: 80), 50)
        XCTAssertEqual(SwipeToReplyMath.clampOffset(80, maxDrag: 80), 80)
    }

    func testBeyondMaxDragRubberBands() {
        // 80 + (200-80)*0.15 = 80 + 18 = 98
        XCTAssertEqual(SwipeToReplyMath.clampOffset(200, maxDrag: 80), 98, accuracy: 0.01)
        XCTAssertGreaterThan(SwipeToReplyMath.clampOffset(500, maxDrag: 80), 80)
        XCTAssertLessThan(SwipeToReplyMath.clampOffset(500, maxDrag: 80), 500)
    }

    func testRepliesOnlyPastThreshold() {
        XCTAssertFalse(SwipeToReplyMath.shouldReply(offset: 40, threshold: 56))
        XCTAssertTrue(SwipeToReplyMath.shouldReply(offset: 56, threshold: 56))
        XCTAssertTrue(SwipeToReplyMath.shouldReply(offset: 80, threshold: 56))
    }

    func testProgressIsClampedZeroToOne() {
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 0, threshold: 56), 0)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 28, threshold: 56), 0.5, accuracy: 0.001)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 56, threshold: 56), 1)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 120, threshold: 56), 1)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 10, threshold: 0), 0)
    }
}
