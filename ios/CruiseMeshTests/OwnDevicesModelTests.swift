import XCTest
@testable import CruiseMesh

/// "Your devices" as arithmetic over core facts, and the two refusals the screen
/// must not offer a tap for (`specs/multi-device-v1.md` §13 WP6).
///
/// The Swift twin of Android's `OwnDevicesModelTest`, case for case: if one
/// shell's list would offer Remove where the other's would not, one of them is
/// showing a button `core_revoke_devices_roster` is about to refuse.
final class OwnDevicesModelTests: XCTestCase {
    private let one = Data([0x01, 0x02, 0x03, 0x04])
    private let two = Data([0x11, 0x22, 0x33, 0x44])
    private let three = Data([0xaa, 0xbb, 0xcc, 0xdd])

    func testRowsCarryPositionApprovalAndWhichDeviceThisIs() {
        let rows = ownDeviceRows(
            deviceIds: [one, two, three],
            approvingDeviceId: one,
            ownDeviceId: two
        )

        XCTAssertEqual(rows.map(\.position), [1, 2, 3])
        XCTAssertEqual(rows.map(\.approves), [true, false, false])
        XCTAssertEqual(rows.map(\.isThisDevice), [false, true, false])
    }

    /// §10.1: only the device holding the roster-signing role can sign the
    /// update, so no other device may offer Remove at all.
    func testOnlyTheApprovingDeviceMayOfferRemove() {
        let fromSibling = ownDeviceRows(
            deviceIds: [one, two, three],
            approvingDeviceId: one,
            ownDeviceId: two
        )
        XCTAssertEqual(fromSibling.map(\.removable), [false, false, false])
        XCTAssertEqual(
            removeBlockedReason(rows: fromSibling, row: fromSibling[2]),
            .notTheApprovingDevice
        )

        let fromApprover = ownDeviceRows(
            deviceIds: [one, two, three],
            approvingDeviceId: one,
            ownDeviceId: one
        )
        XCTAssertEqual(fromApprover.map(\.removable), [false, true, true])
    }

    /// `core_revoke_devices_roster`: "the approving device cannot revoke itself;
    /// that takes the recovery material" (§14.2).
    func testTheApprovingDeviceCannotRemoveItself() {
        let rows = ownDeviceRows(deviceIds: [one, two], approvingDeviceId: one, ownDeviceId: one)
        XCTAssertFalse(rows[0].removable)
        XCTAssertEqual(removeBlockedReason(rows: rows, row: rows[0]), .isTheApprovingDevice)
    }

    /// `core_revoke_devices_roster`: "a person must keep at least one device".
    func testTheLastDeviceIsNeverRemovable() {
        let rows = ownDeviceRows(deviceIds: [one], approvingDeviceId: one, ownDeviceId: one)
        XCTAssertFalse(rows[0].removable)
        XCTAssertEqual(removeBlockedReason(rows: rows, row: rows[0]), .lastDevice)
    }

    /// An install that has never linked has no row that is "this device" — and
    /// that is the ordinary case, not a failure.
    func testAnInstallWithNoDeviceKeysMarksNoRowAsItself() {
        let rows = ownDeviceRows(deviceIds: [one, two], approvingDeviceId: one, ownDeviceId: nil)
        XCTAssertEqual(rows.map(\.isThisDevice), [false, false])
        XCTAssertEqual(rows.map(\.removable), [false, false])
    }

    func testShapeSeparatesNeverLinkedFromASingleListedDevice() {
        XCTAssertEqual(yourDevicesShape(hasRoster: false, rows: []), .neverLinked)
        let single = ownDeviceRows(deviceIds: [one], approvingDeviceId: one, ownDeviceId: one)
        XCTAssertEqual(yourDevicesShape(hasRoster: true, rows: single), .onlyThisDevice)
        let pair = ownDeviceRows(deviceIds: [one, two], approvingDeviceId: one, ownDeviceId: one)
        XCTAssertEqual(yourDevicesShape(hasRoster: true, rows: pair), .several)
    }

    /// The code shown is the tail of a public device id, spaced — never the whole
    /// wall of hex, and never nothing at all when two phones share a name.
    func testShortDeviceCodeIsTheLastFourBytesSpaced() {
        XCTAssertEqual(shortDeviceCode("0011223344556677"), "4455 6677")
        XCTAssertEqual(deviceIdHex(Data([0x00, 0x0f, 0xff])), "000fff")
    }
}
