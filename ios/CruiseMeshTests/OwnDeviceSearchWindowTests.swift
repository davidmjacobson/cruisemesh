import XCTest
@testable import CruiseMesh

/// The sweep motive that goes looking for one of this person's own devices has
/// to stop, exactly as the contact-side motive decays.
///
/// `specs/multi-device-v1.md` §10 step 5. A second phone that is switched off or
/// left at home is missing forever; unbounded, this would sweep the /24 every
/// five minutes on every Wi-Fi the person joins, on battery, for the whole life
/// of each join — and it would do so for precisely the multi-device households
/// the mechanism was added for.
///
/// Mirrors Android's `OwnDeviceSearchWindowTest`.
final class OwnDeviceSearchWindowTests: XCTestCase {

    private let windowMs = coreLanOwnDeviceSearchWindowMs()

    private func roster(_ ids: [UInt8]) -> String {
        ownRosterFingerprint(deviceIds: ids.map { Data([$0]) })
    }

    func testASiblingThatGoesMissingIsSearchedForAndTheSearchRunsOut() {
        let window = OwnDeviceSearchWindow()
        let fleet = roster([1, 2])

        // Both devices linked: nothing to look for. (The first observation is a
        // roster change, so it arms; the second, unchanged, does not.)
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 0, nowMs: 0)
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 0, nowMs: windowMs)
        XCTAssertFalse(window.isLive)

        // The sibling link drops.
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 1, nowMs: windowMs)
        XCTAssertTrue(window.isLive)

        window.observe(
            rosterFingerprint: fleet,
            unlinkedOwnDevices: 1,
            nowMs: windowMs + windowMs - 1
        )
        XCTAssertTrue(window.isLive)
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 1, nowMs: windowMs * 4)
        XCTAssertFalse(window.isLive, "a phone left at home kept the subnet sweep running")
    }

    func testARemovalSendsTheApprovingPhoneLookingForTheDeviceItRemoved() {
        let window = OwnDeviceSearchWindow()
        window.observe(rosterFingerprint: roster([1, 2]), unlinkedOwnDevices: 0, nowMs: 0)
        window.observe(rosterFingerprint: roster([1, 2]), unlinkedOwnDevices: 0, nowMs: windowMs)
        XCTAssertFalse(window.isLive)

        // "Remove device": the removed one leaves this phone's roster at once,
        // so the shortfall does not rise -- it stays zero. Only the roster
        // change itself can send this phone looking for the device it must
        // still hand §10 step 5's notice to.
        window.observe(rosterFingerprint: roster([1]), unlinkedOwnDevices: 0, nowMs: windowMs)
        XCTAssertTrue(window.isLive)

        // Bounded like every other reason to search.
        window.observe(rosterFingerprint: roster([1]), unlinkedOwnDevices: 0, nowMs: windowMs * 2)
        XCTAssertFalse(window.isLive)
    }

    func testJoiningAWiFiNetworkIsAFreshReasonToLook() {
        let window = OwnDeviceSearchWindow()
        window.observe(rosterFingerprint: roster([1, 2]), unlinkedOwnDevices: 1, nowMs: 0)
        window.observe(rosterFingerprint: roster([1, 2]), unlinkedOwnDevices: 1, nowMs: windowMs * 3)
        XCTAssertFalse(window.isLive)

        window.rearm(nowMs: windowMs * 3)
        XCTAssertTrue(window.isLive)
    }

    func testAThreeDevicePersonDoesNotSweepForever() {
        // The transport keeps one own-device link at a time, so with three
        // devices the shortfall can never reach zero. A bare "someone is
        // missing" motive would therefore be permanent, not merely long.
        let window = OwnDeviceSearchWindow()
        let fleet = roster([1, 2, 3])
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 2, nowMs: 0)
        XCTAssertTrue(window.isLive)
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 1, nowMs: windowMs - 1)
        XCTAssertTrue(window.isLive)
        window.observe(rosterFingerprint: fleet, unlinkedOwnDevices: 1, nowMs: windowMs)
        XCTAssertFalse(window.isLive)
    }

    func testTheRosterFingerprintIgnoresOrderAndNoticesARemoval() {
        XCTAssertEqual(roster([2, 1]), roster([1, 2]))
        XCTAssertNotEqual(roster([1, 2]), roster([1]))
    }

    func testClearingForgetsTheSearch() {
        let window = OwnDeviceSearchWindow()
        window.observe(rosterFingerprint: roster([1, 2]), unlinkedOwnDevices: 1, nowMs: 0)
        XCTAssertTrue(window.isLive)
        window.clear()
        XCTAssertFalse(window.isLive)
    }
}
