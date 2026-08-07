import XCTest
@testable import CruiseMesh

/// Shell-side cover for the failover resume debounce — the coalescing rule
/// itself is pinned in the core's own tests; these assert the shape
/// `MeshController.scheduleFailoverResume` depends on, and mirror
/// `FailoverResumeDebounceTest.kt` on Android.
final class FailoverResumeDebounceTests: XCTestCase {

    func testDefaultWindowOutlastsTheObservedDisconnectBurst() {
        // 2026-08-07 capture: one radio event's BLE disconnect callbacks
        // arrived spread over ~240ms. A shorter window would let the resume
        // fan out into a link whose own disconnect is still in flight.
        XCTAssertGreaterThan(FailoverResumeDebounce().windowMs, 240)
    }

    func testOneRadioEventProducesExactlyOneResume() {
        let debounce = FailoverResumeDebounce(windowMs: 300)
        let peer = "aabbccdd"
        let arm = debounce.request(key: peer, nowMs: 1_000)
        XCTAssertEqual(arm?.delayMs, 300)
        XCTAssertNil(debounce.request(key: peer, nowMs: 1_090))
        XCTAssertNil(debounce.request(key: peer, nowMs: 1_240))
        XCTAssertTrue(debounce.isPending(key: peer))

        debounce.fired(key: peer, token: arm?.token ?? 0)
        XCTAssertFalse(debounce.isPending(key: peer))
        // A genuinely later failover is a new burst.
        XCTAssertEqual(debounce.request(key: peer, nowMs: 5_000)?.delayMs, 300)
    }

    func testTwoPeersFailingOverTogetherEachGetTheirOwnResume() {
        let debounce = FailoverResumeDebounce(windowMs: 300)
        XCTAssertEqual(debounce.request(key: "peer-a", nowMs: 0)?.delayMs, 300)
        XCTAssertEqual(debounce.request(key: "peer-b", nowMs: 5)?.delayMs, 300)
    }

    func testAStaleTimerCannotClearANewlyArmedWindow() {
        let debounce = FailoverResumeDebounce(windowMs: 300)
        let first = debounce.request(key: "peer", nowMs: 0)
        let second = debounce.request(key: "peer", nowMs: 300)
        XCTAssertNotEqual(first?.token, second?.token)

        // The first timer runs after the window was re-armed. Clearing the new
        // window's marker here is what would let one burst resume twice.
        debounce.fired(key: "peer", token: first?.token ?? 0)
        XCTAssertTrue(debounce.isPending(key: "peer"))
        XCTAssertNil(debounce.request(key: "peer", nowMs: 310))
    }

    func testClearDropsPendingWindows() {
        let debounce = FailoverResumeDebounce(windowMs: 300)
        XCTAssertEqual(debounce.request(key: "peer", nowMs: 0)?.delayMs, 300)
        debounce.clear()
        XCTAssertFalse(debounce.isPending(key: "peer"))
        XCTAssertEqual(debounce.request(key: "peer", nowMs: 10)?.delayMs, 300)
    }

    func testMonotonicClockIsUsedForTheWindow() {
        // The window and the `asyncAfter` timer must read the same clock: a
        // wall-clock marker paired with a monotonic timer desynchronises on any
        // time correction, which splits one burst into two resumes.
        let first = FailoverResumeDebounce.monotonicNowMs
        let second = FailoverResumeDebounce.monotonicNowMs
        XCTAssertGreaterThanOrEqual(second, first)
        // Uptime, not epoch: nowhere near a 2026 wall-clock millisecond count.
        XCTAssertLessThan(first, 1_000_000_000_000)
    }
}
