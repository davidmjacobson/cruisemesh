import Foundation
import XCTest
@testable import CruiseMesh

/// The names a family gives its own devices, and the day this phone first saw
/// each one — both local, and neither ever a roster field (DL-5).
///
/// The Swift twin of Android's `DeviceNameStore` behaviour.
final class DeviceNameStoreTests: XCTestCase {
    private var suiteName = ""
    private var defaults = UserDefaults.standard

    override func setUpWithError() throws {
        suiteName = "DeviceNameStoreTests.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    func testANameIsRememberedAndTrimmed() {
        DeviceNameStore.setName(deviceIdHex: "aa", name: "  Emma's iPad  ", defaults: defaults)
        XCTAssertEqual(DeviceNameStore.name(deviceIdHex: "aa", defaults: defaults), "Emma's iPad")
    }

    func testClearingANameRemovesItRatherThanStoringBlankness() {
        DeviceNameStore.setName(deviceIdHex: "aa", name: "Old phone", defaults: defaults)
        DeviceNameStore.setName(deviceIdHex: "aa", name: "   ", defaults: defaults)
        XCTAssertNil(DeviceNameStore.name(deviceIdHex: "aa", defaults: defaults))
    }

    /// First-seen, not added-at: a phone that joins a fleet of three learns about
    /// two devices added long before it existed, and claiming those dates as
    /// their own would be inventing a fact.
    func testFirstSightingNeverMovesOnceRecorded() {
        XCTAssertEqual(
            DeviceNameStore.rememberSeen(deviceIdHex: "bb", nowMs: 1_000, defaults: defaults),
            1_000
        )
        XCTAssertEqual(
            DeviceNameStore.rememberSeen(deviceIdHex: "bb", nowMs: 9_000, defaults: defaults),
            1_000
        )
        XCTAssertEqual(DeviceNameStore.firstSeenMs(deviceIdHex: "bb", defaults: defaults), 1_000)
    }

    func testADeviceThisPhoneHasNeverSeenHasNoSighting() {
        XCTAssertNil(DeviceNameStore.firstSeenMs(deviceIdHex: "cc", defaults: defaults))
    }

    /// Nothing here outlives the roster: removing a device forgets both notes.
    func testForgettingADeviceClearsBothNotes() {
        DeviceNameStore.setName(deviceIdHex: "dd", name: "Spare", defaults: defaults)
        _ = DeviceNameStore.rememberSeen(deviceIdHex: "dd", nowMs: 42, defaults: defaults)
        DeviceNameStore.forget(deviceIdHex: "dd", defaults: defaults)
        XCTAssertNil(DeviceNameStore.name(deviceIdHex: "dd", defaults: defaults))
        XCTAssertNil(DeviceNameStore.firstSeenMs(deviceIdHex: "dd", defaults: defaults))
    }
}
