import CoreBluetooth
import UserNotifications
import XCTest
@testable import CruiseMesh

final class OnboardingPermissionsTests: XCTestCase {
    func testAsksWhileAnythingIsStillUndecided() {
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .notDetermined, notifications: .notDetermined),
            .request
        )
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .allowedAlways, notifications: .notDetermined),
            .request
        )
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .notDetermined, notifications: .authorized),
            .request
        )
        // Mixed: one refused, one still open. The open one is still worth a
        // prompt, and Settings comes next once it has been answered.
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .notDetermined, notifications: .denied),
            .request
        )
    }

    func testDeniedPermissionSendsToSettingsRatherThanReAsking() {
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .denied, notifications: .denied),
            .openSettings
        )
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .restricted, notifications: .authorized),
            .openSettings
        )
        // Mixed, both decided: one grant does not make a dead re-request button
        // acceptable for the other.
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .allowedAlways, notifications: .denied),
            .openSettings
        )
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .denied, notifications: .authorized),
            .openSettings
        )
    }

    func testEverythingGrantedOffersNoAction() {
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .allowedAlways, notifications: .authorized),
            .allSet
        )
        // Provisional and ephemeral notifications still deliver, so they are
        // not a reason to send anyone to Settings.
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .allowedAlways, notifications: .provisional),
            .allSet
        )
        XCTAssertEqual(
            OnboardingPermissions.action(bluetooth: .allowedAlways, notifications: .ephemeral),
            .allSet
        )
    }

    func testBlockedFlagsDriveTheExplanation() {
        XCTAssertTrue(OnboardingPermissions.isBluetoothBlocked(.denied))
        XCTAssertTrue(OnboardingPermissions.isBluetoothBlocked(.restricted))
        XCTAssertFalse(OnboardingPermissions.isBluetoothBlocked(.notDetermined))
        XCTAssertFalse(OnboardingPermissions.isBluetoothBlocked(.allowedAlways))

        XCTAssertTrue(OnboardingPermissions.areNotificationsBlocked(.denied))
        XCTAssertFalse(OnboardingPermissions.areNotificationsBlocked(.notDetermined))
        XCTAssertFalse(OnboardingPermissions.areNotificationsBlocked(.authorized))
        XCTAssertFalse(OnboardingPermissions.areNotificationsBlocked(.provisional))
    }
}
