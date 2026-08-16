import Foundation

/**
 The three one-time facts behind the "Before you sail" checklist that nothing
 else in the app already records.

 Two of them are proof that something happened once (a message arrived without
 the internet; a backup file was written) and are deliberately never cleared: a
 step that unticks itself after a quiet afternoon would teach people to ignore
 the whole screen. The third is the home-screen card's dismissal, which is
 presentation and stays in the shell -- the core policy has no opinion about it.
 */

/// Has a message ever reached this phone without the internet?
enum OfflineDeliverySeenStore {
    private static let seenKey = "cruisemesh.sail.offlineDeliverySeen"

    /// Did this arrival reach us without the internet?
    ///
    /// The classification is the core's (`corePeerTransportForArrival`), so
    /// neither shell re-decides which `MessageArrival.transport` codes mean "no
    /// internet was involved". Everything except the Shore Pass path counts,
    /// carried arrivals included: a message another phone muled over Bluetooth
    /// still made its last hop with the internet out, which is exactly what the
    /// step asks a family to see for themselves. Core also folds any code it
    /// does not recognise into Shore Pass, so an encoding added later cannot
    /// tick this step by default.
    static func isNearby(transport: UInt8) -> Bool {
        corePeerTransportForArrival(transport: transport) != .shorePass
    }

    static func hasSeen(defaults: UserDefaults = AppDefaults.current) -> Bool {
        defaults.bool(forKey: seenKey)
    }

    /// Records one arrival. Cheap enough for the receive path: an internet
    /// arrival returns before touching defaults, and once the flag is set every
    /// later call returns on the same read.
    static func noteArrival(transport: UInt8, defaults: UserDefaults = AppDefaults.current) {
        guard isNearby(transport: transport) else { return }
        guard !defaults.bool(forKey: seenKey) else { return }
        defaults.set(true, forKey: seenKey)
    }
}

/// Has an encrypted backup file ever been written from this phone?
enum BackupCreatedStore {
    private static let createdKey = "cruisemesh.sail.backupCreated"

    static func hasCreated(defaults: UserDefaults = AppDefaults.current) -> Bool {
        defaults.bool(forKey: createdKey)
    }

    /// Written when the export sheet reports the file saved, not when the
    /// backup bytes are built: a backup that was prepared and then cancelled is
    /// not a backup anyone can restore from.
    static func markCreated(defaults: UserDefaults = AppDefaults.current) {
        defaults.set(true, forKey: createdKey)
    }
}

/// Has the home-screen checklist card been dismissed?
///
/// One flag, no expiry. The card also disappears on its own once the required
/// steps are done, so this only has to answer for the person who wants it gone
/// before then; the checklist itself stays reachable from Settings either way.
enum SailChecklistCardStore {
    private static let dismissedKey = "cruisemesh.sail.cardDismissed"

    static func isDismissed(defaults: UserDefaults = AppDefaults.current) -> Bool {
        defaults.bool(forKey: dismissedKey)
    }

    static func dismiss(defaults: UserDefaults = AppDefaults.current) {
        defaults.set(true, forKey: dismissedKey)
    }
}
