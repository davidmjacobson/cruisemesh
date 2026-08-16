import CoreBluetooth
import UserNotifications
import XCTest
@testable import CruiseMesh

/// The shell half of the "Before you sail" checklist: turning this platform's
/// permission enums into the core's plain booleans, and the card policy the
/// core deliberately has no opinion about. The policy itself is tested in
/// `core/src/sail_checklist.rs`; what is checked here is that iOS hands it the
/// right facts and reads its answer back correctly.
final class SailChecklistLogicTests: XCTestCase {

    private func facts(
        contactCount: Int = 0,
        shorePassConfigured: Bool = false,
        bluetooth: CBManagerAuthorization = .notDetermined,
        notifications: UNAuthorizationStatus = .notDetermined,
        offlineDeliverySeen: Bool = false,
        backupCreated: Bool = false
    ) -> SailChecklistFacts {
        SailChecklistFacts(
            contactCount: contactCount,
            shorePassConfigured: shorePassConfigured,
            bluetooth: bluetooth,
            notifications: notifications,
            offlineDeliverySeen: offlineDeliverySeen,
            backupCreated: backupCreated
        )
    }

    /// Everything the required steps need, and nothing optional.
    private var readyFacts: SailChecklistFacts {
        facts(
            contactCount: 2,
            bluetooth: .allowedAlways,
            notifications: .authorized,
            offlineDeliverySeen: true
        )
    }

    private func item(
        _ id: CoreSailChecklistItemId,
        in report: CoreSailChecklistReport
    ) -> CoreSailChecklistItem? {
        report.items.first { $0.id == id }
    }

    // MARK: - Permission mapping

    func testOnlyAnAllowedAlwaysAuthorizationCountsAsBluetooth() {
        XCTAssertTrue(SailChecklistInputs.isBluetoothGranted(.allowedAlways))
        XCTAssertFalse(SailChecklistInputs.isBluetoothGranted(.notDetermined))
        XCTAssertFalse(SailChecklistInputs.isBluetoothGranted(.denied))
        XCTAssertFalse(SailChecklistInputs.isBluetoothGranted(.restricted))
    }

    func testProvisionalAndEphemeralNotificationsStillDeliver() {
        XCTAssertTrue(SailChecklistInputs.areNotificationsGranted(.authorized))
        XCTAssertTrue(SailChecklistInputs.areNotificationsGranted(.provisional))
        XCTAssertTrue(SailChecklistInputs.areNotificationsGranted(.ephemeral))
        XCTAssertFalse(SailChecklistInputs.areNotificationsGranted(.notDetermined))
        XCTAssertFalse(SailChecklistInputs.areNotificationsGranted(.denied))
    }

    // MARK: - Inputs handed to the core

    func testFactsMapOntoTheCoreInput() {
        let input = SailChecklistInputs.input(
            from: facts(
                contactCount: 3,
                shorePassConfigured: true,
                bluetooth: .allowedAlways,
                notifications: .denied,
                offlineDeliverySeen: true,
                backupCreated: true
            )
        )
        XCTAssertEqual(input.contactCount, 3)
        XCTAssertTrue(input.shorePassConfigured)
        XCTAssertTrue(input.bluetoothPermission)
        XCTAssertFalse(input.notificationsPermission)
        XCTAssertTrue(input.offlineDeliverySeen)
        XCTAssertTrue(input.backupCreated)
    }

    /// The single most damaging thing this seam could get wrong: passing
    /// `false` for a grant iOS does not have would leave the permissions step,
    /// and therefore the whole checklist, permanently unfinishable.
    func testBatteryExemptionIsAbsentRatherThanRefusedOnIos() {
        XCTAssertNil(SailChecklistInputs.input(from: readyFacts).batteryOptimizationExempt)

        let report = SailChecklistInputs.report(for: readyFacts)
        XCTAssertFalse(report.permissions.contains { $0.permission == .batteryOptimization })
        XCTAssertEqual(report.permissions.map(\.permission), [.bluetooth, .notifications])
        XCTAssertEqual(item(.permissions, in: report)?.done, true)
        XCTAssertTrue(report.ready)
    }

    // MARK: - Reading the core's answer

    func testNothingDoneOnAFreshPhone() {
        let report = SailChecklistInputs.report(for: facts())
        XCTAssertFalse(report.ready)
        XCTAssertEqual(report.doneCount, 0)
        XCTAssertEqual(report.totalCount, 5)
        XCTAssertEqual(
            report.items.map(\.id),
            [.shorePass, .addFamily, .permissions, .offlineTest, .backup]
        )
        XCTAssertTrue(report.items.allSatisfy { !$0.done })
    }

    func testOneContactIsEnoughToAddFamily() {
        XCTAssertEqual(item(.addFamily, in: SailChecklistInputs.report(for: facts()))?.done, false)
        XCTAssertEqual(
            item(.addFamily, in: SailChecklistInputs.report(for: facts(contactCount: 1)))?.done,
            true
        )
    }

    func testEitherMissingGrantHoldsThePermissionsStepOpen() {
        let noBluetooth = SailChecklistInputs.report(
            for: facts(contactCount: 1, bluetooth: .denied, notifications: .authorized)
        )
        XCTAssertEqual(item(.permissions, in: noBluetooth)?.done, false)
        XCTAssertEqual(
            noBluetooth.permissions.first { $0.permission == .bluetooth }?.granted,
            false
        )

        let noNotifications = SailChecklistInputs.report(
            for: facts(contactCount: 1, bluetooth: .allowedAlways, notifications: .denied)
        )
        XCTAssertEqual(item(.permissions, in: noNotifications)?.done, false)
        XCTAssertEqual(
            noNotifications.permissions.first { $0.permission == .notifications }?.granted,
            false
        )
    }

    func testInternetDeliveryNeverTicksTheOfflineTest() {
        // `offlineDeliverySeen` is the only thing that can: a Shore Pass and a
        // full contact list say nothing about whether this phone has worked
        // without the internet.
        let report = SailChecklistInputs.report(
            for: facts(
                contactCount: 4,
                shorePassConfigured: true,
                bluetooth: .allowedAlways,
                notifications: .authorized
            )
        )
        XCTAssertEqual(item(.offlineTest, in: report)?.done, false)
        XCTAssertFalse(report.ready)
    }

    func testOptionalStepsNeverGateReady() {
        let report = SailChecklistInputs.report(for: readyFacts)
        XCTAssertTrue(report.ready)
        XCTAssertEqual(item(.shorePass, in: report)?.done, false)
        XCTAssertEqual(item(.shorePass, in: report)?.required, false)
        XCTAssertEqual(item(.backup, in: report)?.done, false)
        XCTAssertEqual(item(.backup, in: report)?.required, false)
        // Ready with two of the five steps still untouched, so the card's own
        // "N of M" must not be mistaken for the sail gate.
        XCTAssertEqual(report.doneCount, 3)
        XCTAssertEqual(report.requiredDoneCount, 3)
        XCTAssertEqual(report.requiredTotalCount, 3)
    }

    // MARK: - The card

    func testTheCardGoesAwayWhenReadyOrWhenDismissed() {
        let unfinished = SailChecklistInputs.report(for: facts())
        let ready = SailChecklistInputs.report(for: readyFacts)

        XCTAssertTrue(SailChecklistCard.isVisible(report: unfinished, dismissed: false))
        XCTAssertFalse(SailChecklistCard.isVisible(report: unfinished, dismissed: true))
        XCTAssertFalse(SailChecklistCard.isVisible(report: ready, dismissed: false))
        XCTAssertFalse(SailChecklistCard.isVisible(report: ready, dismissed: true))
    }

    /// An optional step left undone must not keep the card on the home screen.
    func testAnUnfinishedOptionalStepDoesNotHoldTheCardOpen() {
        let report = SailChecklistInputs.report(for: readyFacts)
        XCTAssertLessThan(report.doneCount, report.totalCount)
        XCTAssertFalse(SailChecklistCard.isVisible(report: report, dismissed: false))
    }

    // MARK: - Copy that changes with the facts

    func testTheFamilyStepNamesTheCountOnceThereIsOne() {
        XCTAssertEqual(
            SailChecklistCopy.subtitle(.addFamily, contactCount: 0, done: false),
            String(localized: "Scan each other's codes in person, before everyone scatters.")
        )
        XCTAssertEqual(SailChecklistCopy.peopleAdded(1), String(localized: "1 person added"))
        XCTAssertEqual(
            SailChecklistCopy.subtitle(.addFamily, contactCount: 1, done: true),
            SailChecklistCopy.peopleAdded(1)
        )
        XCTAssertNotEqual(SailChecklistCopy.peopleAdded(2), SailChecklistCopy.peopleAdded(1))
    }

    func testTheOfflineTestStopsGivingInstructionsOnceItIsDone() {
        XCTAssertNotEqual(
            SailChecklistCopy.subtitle(.offlineTest, contactCount: 1, done: true),
            SailChecklistCopy.subtitle(.offlineTest, contactCount: 1, done: false)
        )
    }

    // MARK: - The one-time facts

    func testOnlyANearbyArrivalProvesTheOfflineTest() {
        // 0/1 Bluetooth direct and carried, 3/4 local Wi-Fi direct and carried.
        // A carried arrival counts: another phone muled it the last hop, which
        // still happened with the internet out.
        for transport: UInt8 in [0, 1, 3, 4] {
            XCTAssertTrue(
                OfflineDeliverySeenStore.isNearby(transport: transport),
                "transport \(transport) should count as nearby"
            )
        }
        // 2 is internet delivery, and an encoding core does not use yet is
        // folded in with it rather than given the benefit of the doubt.
        XCTAssertFalse(OfflineDeliverySeenStore.isNearby(transport: 2))
        XCTAssertFalse(OfflineDeliverySeenStore.isNearby(transport: 9))
    }

    func testAnInternetArrivalLeavesTheFlagAloneAndANearbyOneSetsItForGood() throws {
        let suiteName = "SailChecklistLogicTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        XCTAssertFalse(OfflineDeliverySeenStore.hasSeen(defaults: defaults))
        OfflineDeliverySeenStore.noteArrival(transport: 2, defaults: defaults)
        XCTAssertFalse(OfflineDeliverySeenStore.hasSeen(defaults: defaults))

        OfflineDeliverySeenStore.noteArrival(transport: 1, defaults: defaults)
        XCTAssertTrue(OfflineDeliverySeenStore.hasSeen(defaults: defaults))
        // A proof already given is not withdrawn by later internet-only traffic.
        OfflineDeliverySeenStore.noteArrival(transport: 2, defaults: defaults)
        XCTAssertTrue(OfflineDeliverySeenStore.hasSeen(defaults: defaults))
    }

    func testBackupAndCardFlagsAreWrittenOnce() throws {
        let suiteName = "SailChecklistLogicTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        XCTAssertFalse(BackupCreatedStore.hasCreated(defaults: defaults))
        BackupCreatedStore.markCreated(defaults: defaults)
        XCTAssertTrue(BackupCreatedStore.hasCreated(defaults: defaults))

        XCTAssertFalse(SailChecklistCardStore.isDismissed(defaults: defaults))
        SailChecklistCardStore.dismiss(defaults: defaults)
        XCTAssertTrue(SailChecklistCardStore.isDismissed(defaults: defaults))
    }

    func testTheStoresDoNotShareAKey() throws {
        let suiteName = "SailChecklistLogicTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        SailChecklistCardStore.dismiss(defaults: defaults)
        XCTAssertFalse(BackupCreatedStore.hasCreated(defaults: defaults))
        XCTAssertFalse(OfflineDeliverySeenStore.hasSeen(defaults: defaults))
    }

    /// The notification grant lands after the rest have been read, and must not
    /// disturb them on the way in.
    func testTheLateNotificationGrantReplacesNothingElse() {
        let base = facts(contactCount: 2, shorePassConfigured: true, bluetooth: .allowedAlways)
        let updated = base.withNotifications(.authorized)
        XCTAssertEqual(updated.notifications, .authorized)
        XCTAssertEqual(updated.contactCount, base.contactCount)
        XCTAssertEqual(updated.shorePassConfigured, base.shorePassConfigured)
        XCTAssertEqual(updated.bluetooth, base.bluetooth)
        XCTAssertEqual(updated.offlineDeliverySeen, base.offlineDeliverySeen)
        XCTAssertEqual(updated.backupCreated, base.backupCreated)
    }
}
