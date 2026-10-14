import XCTest
@testable import CruiseMesh

/// §9.4's radio silence, as the mesh controller holds it.
///
/// The scenario pinned here: a new device is inside its pre-activation window,
/// so the controller has taken the mesh down. The person then opens the chat
/// list (or an APNs relay wake arrives, or the app is relaunched for BLE), and
/// that calls `MeshController.start()`. Before this latch existed, that start
/// brought BLE advertising, Bonjour and the relay loop straight back up, and
/// nothing took them down again because the gate had not changed.
///
/// `MeshController` itself is a process-wide singleton driving real radios, so
/// what is asserted is the decision it now asks `LinkSilenceLatch` for; the
/// controller's only part is to obey it. The Swift counterpart of Android's
/// `LinkRadioPlanTest`.
final class LinkSilenceLatchTests: XCTestCase {
    func testAStartDuringSilenceDoesNotBringTheRadiosUp() {
        var latch = LinkSilenceLatch()
        XCTAssertEqual(latch.apply(allowed: false, isRunning: true), .stopMesh)

        // The chat list appearing, an APNs wake, a deep link: all land here.
        XCTAssertFalse(latch.admitStart(gateAllowed: false))
        XCTAssertFalse(latch.admitStart(gateAllowed: false))

        // A refresh that finds the gate still shut has nothing to undo.
        XCTAssertEqual(latch.apply(allowed: false, isRunning: false), .nothing)
        XCTAssertTrue(latch.silenced)
    }

    func testTheEndOfTheWindowRestartsAMeshItSilenced() {
        var latch = LinkSilenceLatch()
        XCTAssertEqual(latch.apply(allowed: false, isRunning: true), .stopMesh)

        XCTAssertEqual(latch.apply(allowed: true, isRunning: false), .startMesh)
        XCTAssertTrue(latch.admitStart(gateAllowed: true))
        XCTAssertFalse(latch.silenced)
    }

    func testAProcessRelaunchedInsideTheWindowStaysQuietThenComesUp() {
        // Killed mid-ceremony: nothing was running, so no apply ever silenced
        // it. The first start still has to be refused, and remembered.
        var latch = LinkSilenceLatch()
        XCTAssertFalse(latch.admitStart(gateAllowed: false))

        XCTAssertEqual(latch.apply(allowed: true, isRunning: false), .startMesh)
    }

    func testAMeshThePersonHadSwitchedOffIsLeftOff() {
        var latch = LinkSilenceLatch()
        XCTAssertEqual(latch.apply(allowed: false, isRunning: false), .nothing)

        XCTAssertEqual(latch.apply(allowed: true, isRunning: false), .nothing)
    }

    func testSwitchingTheMeshOffDuringSilenceIsHonouredWhenTheWindowEnds() {
        var latch = LinkSilenceLatch()
        XCTAssertEqual(latch.apply(allowed: false, isRunning: true), .stopMesh)

        latch.personStopped()

        XCTAssertEqual(latch.apply(allowed: true, isRunning: false), .nothing)
    }

    func testARunningMeshTheGateNoLongerAllowsIsStoppedOnAnyRefresh() {
        // The gate shut without the controller hearing of it (say a start
        // slipped through first). A refresh that finds the gate unchanged
        // still has to put the radios down.
        var latch = LinkSilenceLatch()
        XCTAssertEqual(latch.apply(allowed: false, isRunning: true), .stopMesh)
        XCTAssertEqual(latch.apply(allowed: false, isRunning: true), .stopMesh)
    }

    func testOutsideAnyWindowEveryStartIsAdmitted() {
        var latch = LinkSilenceLatch()
        XCTAssertTrue(latch.admitStart(gateAllowed: true))
        XCTAssertEqual(latch.apply(allowed: true, isRunning: true), .nothing)
        XCTAssertFalse(latch.silenced)
    }

    func testAFreshInstallMayAdvertise() throws {
        // The gate read the controller now makes before every start: an install
        // that has never linked a device must not be held down by it.
        let store = try MessageStore.open(path: ":memory:")

        XCTAssertTrue(LinkVisibility.readGate(store: store))
        XCTAssertTrue(LinkVisibility.mayAdvertise())
    }
}
