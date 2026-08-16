import CoreBluetooth
import Foundation
import UserNotifications

/**
 Everything the "Before you sail" checklist decides before it touches SwiftUI.

 The policy itself is not here -- it is in the core (`core/src/sail_checklist.rs`),
 so iOS and Android cannot disagree about what is done, what is optional, or
 what "ready" means. What lives here is the narrow shell-side work the core
 deliberately does not do: turning this platform's permission enums into the
 core's plain booleans, and turning the core's answer into copy.

 Nothing in this file imports SwiftUI, so all of it is unit tested directly.
 */

// MARK: - Inputs

/// The facts this phone can state about itself, gathered before the core is
/// asked anything.
///
/// Held as the platform's own enums rather than as booleans so the mapping is
/// in one testable place: which notification authorizations still deliver, and
/// which Bluetooth authorization actually lets us use the radio, are exactly
/// the questions a shell gets wrong quietly.
struct SailChecklistFacts: Equatable {
    let contactCount: Int
    let shorePassConfigured: Bool
    let bluetooth: CBManagerAuthorization
    let notifications: UNAuthorizationStatus
    let offlineDeliverySeen: Bool
    let backupCreated: Bool

    /// What is true before anything has been read. Everything absent, which is
    /// the honest first frame: no step claims to be done until its fact has
    /// actually been looked up.
    static let unknown = SailChecklistFacts(
        contactCount: 0,
        shorePassConfigured: false,
        bluetooth: .notDetermined,
        notifications: .notDetermined,
        offlineDeliverySeen: false,
        backupCreated: false
    )

    /// The notification grant is the one fact iOS will not answer
    /// synchronously, so it lands on its own after the rest have been read.
    func withNotifications(_ status: UNAuthorizationStatus) -> SailChecklistFacts {
        SailChecklistFacts(
            contactCount: contactCount,
            shorePassConfigured: shorePassConfigured,
            bluetooth: bluetooth,
            notifications: status,
            offlineDeliverySeen: offlineDeliverySeen,
            backupCreated: backupCreated
        )
    }
}

enum SailChecklistInputs {

    /// Bluetooth is granted only when iOS will actually let us use the radio.
    /// `.notDetermined` is not a grant, and neither is a restricted profile.
    static func isBluetoothGranted(_ authorization: CBManagerAuthorization) -> Bool {
        authorization == .allowedAlways
    }

    /// Provisional and ephemeral authorizations still deliver notifications, so
    /// they count as granted here -- the same reading the onboarding slide
    /// uses. An unrecognised future case is not assumed to deliver.
    static func areNotificationsGranted(_ status: UNAuthorizationStatus) -> Bool {
        switch status {
        case .authorized, .provisional, .ephemeral: return true
        default: return false
        }
    }

    /**
     The core's input record.

     `batteryOptimizationExempt` is `nil` and always will be on iOS: there is no
     counterpart to Android's exemption here, and core reads the absence as
     "this platform has no such grant" -- so the row never appears and can never
     hold the permissions step open. Passing `false` instead would silently make
     the checklist unfinishable on every iPhone.
     */
    static func input(from facts: SailChecklistFacts) -> CoreSailChecklistInput {
        CoreSailChecklistInput(
            contactCount: UInt64(max(0, facts.contactCount)),
            shorePassConfigured: facts.shorePassConfigured,
            bluetoothPermission: isBluetoothGranted(facts.bluetooth),
            notificationsPermission: areNotificationsGranted(facts.notifications),
            batteryOptimizationExempt: nil,
            offlineDeliverySeen: facts.offlineDeliverySeen,
            backupCreated: facts.backupCreated
        )
    }

    static func report(for facts: SailChecklistFacts) -> CoreSailChecklistReport {
        coreSailChecklist(input: input(from: facts))
    }
}

// MARK: - Card policy

enum SailChecklistCard {
    /// The home-screen card shows until the required steps are done, or until
    /// someone dismisses it. Optional steps never keep it on screen: the card
    /// exists to get a family sailing, not to nag about a backup.
    static func isVisible(report: CoreSailChecklistReport, dismissed: Bool) -> Bool {
        !report.ready && !dismissed
    }
}

// MARK: - Copy

/**
 Every user-facing word on the checklist.

 Nothing here decides anything: it turns the core's enums and counts into copy.
 House style applies throughout -- sentence case, family-simple words, no
 protocol jargon, `Shore Pass` for internet delivery, and no promises about
 whether a message will get through.
 */
enum SailChecklistCopy {

    /// The line under the title. Ready is a full stop rather than a
    /// congratulation: the screen stays reachable and the optional steps stay
    /// listed, so there is nothing to celebrate away.
    static func intro(ready: Bool) -> String {
        ready
            ? String(localized: "You're set to sail.")
            : String(localized: "Get set up while everyone's still together. Each step checks itself off as it's done.")
    }

    static func progress(done: UInt32, total: UInt32) -> String {
        String(localized: "\(Int(done)) of \(Int(total)) done")
    }

    static func title(_ id: CoreSailChecklistItemId) -> String {
        switch id {
        case .shorePass: return String(localized: "Set up your Shore Pass")
        case .addFamily: return String(localized: "Add your family")
        case .permissions: return String(localized: "Let it run in your pocket")
        case .offlineTest: return String(localized: "Send a message with no internet")
        case .backup: return String(localized: "Back up your identity")
        }
    }

    /**
     The line under a step's title.

     Two of them change with the facts rather than with the tick: the family
     step names the count once there is one to name, and the offline test
     stops giving instructions for something already done.
     */
    static func subtitle(
        _ id: CoreSailChecklistItemId,
        contactCount: Int,
        done: Bool
    ) -> String {
        switch id {
        case .shorePass:
            return String(localized: "Optional. If you bought one, add it before trading codes.")
        case .addFamily:
            guard contactCount > 0 else {
                return String(localized: "Scan each other's codes in person, before everyone scatters.")
            }
            return peopleAdded(contactCount)
        case .permissions:
            return String(localized: "Bluetooth and notifications, so CruiseMesh can keep working with the screen off.")
        case .offlineTest:
            return done
                ? String(localized: "A message has already arrived here without the internet.")
                : String(localized: "Put two phones in airplane mode, turn Bluetooth back on, and send a message.")
        case .backup:
            return String(localized: "Optional. Save an encrypted copy in case you lose your phone.")
        }
    }

    static func peopleAdded(_ count: Int) -> String {
        count == 1
            ? String(localized: "1 person added")
            : String(localized: "\(count) people added")
    }

    /// `batteryOptimization` never reaches an iPhone -- core drops the row when
    /// the input is absent -- but the switch has to answer for it, and a title
    /// is a cheaper answer than a crash if that ever changes.
    static func permissionTitle(_ permission: CoreSailPermission) -> String {
        switch permission {
        case .bluetooth: return String(localized: "Bluetooth")
        case .notifications: return String(localized: "Notifications")
        case .batteryOptimization: return String(localized: "Background power")
        }
    }

    static func permissionStatus(granted: Bool) -> String {
        granted
            ? String(localized: "Allowed")
            : String(localized: "Not allowed yet")
    }

    /// Read out for the tick beside a step, which otherwise says nothing at
    /// all to a screen reader.
    static func statusLabel(done: Bool) -> String {
        done
            ? String(localized: "Done")
            : String(localized: "Not done yet")
    }
}
