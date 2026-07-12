import Foundation

enum ProfileStore {
    private static let displayNameKey = "cruisemesh.displayName"
    private static let ownAvatarEpochKey = "cruisemesh.ownAvatarEpoch"

    static func loadDisplayName() -> String {
        UserDefaults.standard.string(forKey: displayNameKey) ?? ""
    }

    static func saveDisplayName(_ name: String) {
        UserDefaults.standard.set(name.trimmingCharacters(in: .whitespacesAndNewlines), forKey: displayNameKey)
    }

    static func loadOwnAvatarEpoch() -> Int64 {
        let value = UserDefaults.standard.object(forKey: ownAvatarEpochKey) as? NSNumber
        return value?.int64Value ?? 0
    }

    @discardableResult
    static func bumpOwnAvatarEpoch() -> Int64 {
        let epoch = Int64(Date().timeIntervalSince1970 * 1_000)
        UserDefaults.standard.set(epoch, forKey: ownAvatarEpochKey)
        return epoch
    }
}
