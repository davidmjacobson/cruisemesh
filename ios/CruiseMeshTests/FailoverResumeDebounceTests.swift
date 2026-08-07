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
        XCTAssertEqual(debounce.request(key: peer, nowMs: 1_000), 300)
        XCTAssertNil(debounce.request(key: peer, nowMs: 1_090))
        XCTAssertNil(debounce.request(key: peer, nowMs: 1_240))
        XCTAssertTrue(debounce.isPending(key: peer))

        debounce.fired(key: peer)
        XCTAssertFalse(debounce.isPending(key: peer))
        // A genuinely later failover is a new burst.
        XCTAssertEqual(debounce.request(key: peer, nowMs: 5_000), 300)
    }

    func testTwoPeersFailingOverTogetherEachGetTheirOwnResume() {
        let debounce = FailoverResumeDebounce(windowMs: 300)
        XCTAssertEqual(debounce.request(key: "peer-a", nowMs: 0), 300)
        XCTAssertEqual(debounce.request(key: "peer-b", nowMs: 5), 300)
    }

    func testClearDropsPendingWindows() {
        let debounce = FailoverResumeDebounce(windowMs: 300)
        XCTAssertEqual(debounce.request(key: "peer", nowMs: 0), 300)
        debounce.clear()
        XCTAssertFalse(debounce.isPending(key: "peer"))
        XCTAssertEqual(debounce.request(key: "peer", nowMs: 10), 300)
    }
}
