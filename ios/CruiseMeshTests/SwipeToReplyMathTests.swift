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

    func testVerticalDragLeavesTheBubbleToTheScrollingThread() {
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: 10, height: -120), alreadyEngaged: false
        ))
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: 10, height: 120), alreadyEngaged: false
        ))
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: 0, height: 60), alreadyEngaged: false
        ))
    }

    func testSidewaysDragEngagesTheBubble() {
        XCTAssertTrue(SwipeToReplyMath.engages(
            translation: CGSize(width: 40, height: 10), alreadyEngaged: false
        ))
    }

    func testLeftwardDragNeverEngages() {
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: -40, height: 0), alreadyEngaged: false
        ))
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: -40, height: 0), alreadyEngaged: true
        ))
    }

    func testEngagedSwipeSurvivesACurvingFinger() {
        XCTAssertTrue(SwipeToReplyMath.engages(
            translation: CGSize(width: 40, height: 200), alreadyEngaged: true
        ))
    }

    func testAVoiceScrubDoesNotEngageSwipeToReply() {
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: 80, height: 0),
            alreadyEngaged: false,
            scrubbing: true
        ))
        XCTAssertFalse(SwipeToReplyMath.engages(
            translation: CGSize(width: 80, height: 0),
            alreadyEngaged: true,
            scrubbing: true
        ))
    }

    func testVoiceSeekDragIsActiveOnlyWhileAScrubIsHeld() {
        // Leave the flag clear even if an earlier test leaked a begin().
        while VoiceSeekDrag.isActive { VoiceSeekDrag.end() }
        XCTAssertFalse(VoiceSeekDrag.isActive)
        VoiceSeekDrag.begin()
        XCTAssertTrue(VoiceSeekDrag.isActive)
        VoiceSeekDrag.begin()
        VoiceSeekDrag.end()
        XCTAssertTrue(VoiceSeekDrag.isActive)
        VoiceSeekDrag.end()
        XCTAssertFalse(VoiceSeekDrag.isActive)
    }

    func testAVoiceScrubDoesNotStartAReplyOnRelease() {
        XCTAssertFalse(SwipeToReplyMath.shouldReply(offset: 80, threshold: 56, scrubbing: true))
        XCTAssertTrue(SwipeToReplyMath.shouldReply(offset: 80, threshold: 56, scrubbing: false))
    }

    func testProgressIsClampedZeroToOne() {
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 0, threshold: 56), 0)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 28, threshold: 56), 0.5, accuracy: 0.001)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 56, threshold: 56), 1)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 120, threshold: 56), 1)
        XCTAssertEqual(SwipeToReplyMath.progress(offset: 10, threshold: 0), 0)
    }
}
