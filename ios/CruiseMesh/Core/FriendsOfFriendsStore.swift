import Foundation

enum FriendsOfFriendsStore {
    private static let enabledKey = "cruisemesh.friendsOfFriends.enabled"
    private static let revisionKey = "cruisemesh.friendsOfFriends.revision"
    private static let directoryRevisionKey = "cruisemesh.friendsOfFriends.directoryRevision"

    static func isEnabled() -> Bool {
        if UserDefaults.standard.object(forKey: enabledKey) == nil { return true }
        return UserDefaults.standard.bool(forKey: enabledKey)
    }

    static func revision() -> UInt64 {
        let existing = (UserDefaults.standard.object(forKey: revisionKey) as? NSNumber)?.uint64Value ?? 0
        if existing > 0 { return existing }
        let initial = UInt64(max(1, Int64(Date().timeIntervalSince1970 * 1_000)))
        UserDefaults.standard.set(NSNumber(value: initial), forKey: revisionKey)
        return initial
    }

    @discardableResult static func setEnabled(_ enabled: Bool) -> UInt64 {
        if isEnabled() == enabled { return revision() }
        let now = UInt64(max(1, Int64(Date().timeIntervalSince1970 * 1_000)))
        let next = max(revision() + 1, now)
        UserDefaults.standard.set(enabled, forKey: enabledKey)
        UserDefaults.standard.set(NSNumber(value: next), forKey: revisionKey)
        return next
    }

    static func nextDirectoryRevision() -> UInt64 {
        let existing = (UserDefaults.standard.object(forKey: directoryRevisionKey) as? NSNumber)?.uint64Value ?? 0
        let now = UInt64(max(1, Int64(Date().timeIntervalSince1970 * 1_000)))
        let next = max(existing + 1, now)
        UserDefaults.standard.set(NSNumber(value: next), forKey: directoryRevisionKey)
        return next
    }
}
