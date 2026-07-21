import XCTest
@testable import CruiseMesh

final class DraftChangeSignalTests: XCTestCase {
    func testSameEmptinessDoesNotNotify() {
        // Same-presence saves -- the reload/receipt storm this class exists to kill.
        XCTAssertFalse(DraftChangeSignal.shouldNotify(previous: "hello", next: "hello there"))
        XCTAssertFalse(DraftChangeSignal.shouldNotify(previous: "hello there", next: "hello"))
        XCTAssertFalse(DraftChangeSignal.shouldNotify(previous: "", next: ""))
        XCTAssertFalse(DraftChangeSignal.shouldNotify(previous: "\n", next: ""))
    }

    func testEmptyToNonEmptyNotifies() {
        XCTAssertTrue(DraftChangeSignal.shouldNotify(previous: "", next: "h"))
        XCTAssertTrue(DraftChangeSignal.shouldNotify(previous: "\n", next: "h"))
    }

    func testNonEmptyToEmptyNotifies() {
        XCTAssertTrue(DraftChangeSignal.shouldNotify(previous: "hello", next: ""))
        XCTAssertTrue(DraftChangeSignal.shouldNotify(previous: "hello", next: "\n"))
    }

    func testTypingAKeystrokeAtATimeNotifiesOnlyOnce() {
        var previous = ""
        var notifyCount = 0
        for next in ["h", "he", "hel", "hell", "hello"] {
            if DraftChangeSignal.shouldNotify(previous: previous, next: next) { notifyCount += 1 }
            previous = next
        }
        XCTAssertEqual(notifyCount, 1, "expected exactly one notify (the empty -> non-empty transition)")
    }
}
