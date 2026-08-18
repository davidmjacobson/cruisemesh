import Foundation

/// The name a person gave one of their own devices, and the day this phone first
/// saw it — both local to this install, deliberately.
///
/// `specs/multi-device-v1.md` §13 WP6 asks the list for a name and an added date,
/// and the roster carries neither. A `DeviceCert` has keys, flags, a signature
/// and an `addedEpoch` — which counts recovery epochs, not days — and no display
/// name at all, because §4's DL-5 keeps a roster to public key material with
/// nothing addressable or personal in it.
///
/// So the words a family reads come from here instead: a nickname they typed on
/// this phone, and the first moment this phone saw that device id in its own
/// roster. Neither ever leaves the device. That is honest and it is small, and
/// the surface says so ("Names you give devices stay on this phone") rather than
/// implying the other phone knows what it has been called.
///
/// Open for a future work package: a device name that travels would be a §8 sync
/// record, not a roster field — the roster stays free of anything DL-5 would have
/// to keep out of a contact's copy.
///
/// Mirrors Android's `DeviceNameStore`.
enum DeviceNameStore {
    private static let namePrefix = "cruisemesh.device.name."
    private static let firstSeenPrefix = "cruisemesh.device.firstSeen."

    /// The name this person typed for `deviceIdHex`, or nil if they have not.
    static func name(deviceIdHex: String, defaults: UserDefaults = AppDefaults.current) -> String? {
        guard let stored = defaults.string(forKey: namePrefix + deviceIdHex) else { return nil }
        let trimmed = stored.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    static func setName(
        deviceIdHex: String,
        name: String,
        defaults: UserDefaults = AppDefaults.current
    ) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            defaults.removeObject(forKey: namePrefix + deviceIdHex)
        } else {
            defaults.set(trimmed, forKey: namePrefix + deviceIdHex)
        }
    }

    /// When this phone first saw `deviceIdHex`, recording `nowMs` the first time
    /// and never moving it afterwards.
    ///
    /// First-seen rather than added-at, and named that way in the copy ("Seen
    /// here since"), because a phone that joins a fleet of three learns about two
    /// devices that were added long before it existed. Claiming those as their
    /// added dates would be inventing a fact.
    @discardableResult
    static func rememberSeen(
        deviceIdHex: String,
        nowMs: Int64,
        defaults: UserDefaults = AppDefaults.current
    ) -> Int64 {
        let key = firstSeenPrefix + deviceIdHex
        let existing = Int64(defaults.integer(forKey: key))
        if existing > 0 { return existing }
        defaults.set(Int(nowMs), forKey: key)
        return nowMs
    }

    /// Stamp every device in a roster this phone has just adopted or applied.
    ///
    /// Called at adoption, never at render. Stamping while drawing a list meant
    /// "first seen" was really "first looked at", so a device that had been in
    /// the roster for a month dated from whenever somebody happened to open
    /// Settings — and merely opening the screen wrote to disk. A device with no
    /// stamp simply shows no date, which is the honest answer for one this phone
    /// learned about before it kept notes.
    static func rememberRoster(
        deviceIdHexes: [String],
        nowMs: Int64,
        defaults: UserDefaults = AppDefaults.current
    ) {
        for hex in deviceIdHexes {
            rememberSeen(deviceIdHex: hex, nowMs: nowMs, defaults: defaults)
        }
    }

    /// The recorded first sighting, or nil if this phone has never seen it.
    static func firstSeenMs(
        deviceIdHex: String,
        defaults: UserDefaults = AppDefaults.current
    ) -> Int64? {
        let stored = Int64(defaults.integer(forKey: firstSeenPrefix + deviceIdHex))
        return stored > 0 ? stored : nil
    }

    /// Forget a device the person removed. Nothing here outlives the roster.
    static func forget(deviceIdHex: String, defaults: UserDefaults = AppDefaults.current) {
        defaults.removeObject(forKey: namePrefix + deviceIdHex)
        defaults.removeObject(forKey: firstSeenPrefix + deviceIdHex)
    }
}
