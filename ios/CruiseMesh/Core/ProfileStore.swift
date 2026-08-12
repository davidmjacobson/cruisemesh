import Foundation

enum ProfileStore {
    private static let displayNameKey = "cruisemesh.displayName"
    private static let ownAvatarEpochKey = "cruisemesh.ownAvatarEpoch"

    static func loadDisplayName() -> String {
        let stored = loadStoredDisplayName()
        return stored.isEmpty ? defaultDisplayName : stored
    }

    static func loadStoredDisplayName() -> String {
        AppDefaults.current.string(forKey: displayNameKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    @discardableResult
    static func saveDisplayName(_ name: String) -> Bool {
        let normalized = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalized.isEmpty else { return false }
        AppDefaults.current.set(normalized, forKey: displayNameKey)
        return true
    }

    static let defaultDisplayName = "CruiseMesh user"

    static func loadOwnAvatarEpoch() -> Int64 {
        let value = AppDefaults.current.object(forKey: ownAvatarEpochKey) as? NSNumber
        return value?.int64Value ?? 0
    }

    @discardableResult
    static func bumpOwnAvatarEpoch() -> Int64 {
        let epoch = Int64(Date().timeIntervalSince1970 * 1_000)
        AppDefaults.current.set(epoch, forKey: ownAvatarEpochKey)
        return epoch
    }

    static func restoreOwnAvatarEpoch(_ epoch: Int64) {
        AppDefaults.current.set(epoch, forKey: ownAvatarEpochKey)
    }
}
