import Foundation

enum FriendsOfFriendsStore {
    private static let enabledKey = "cruisemesh.friendsOfFriends.enabled"
    private static let revisionKey = "cruisemesh.friendsOfFriends.revision"
    private static let directoryRevisionKey = "cruisemesh.friendsOfFriends.directoryRevision"

    static func isEnabled() -> Bool {
        if AppDefaults.current.object(forKey: enabledKey) == nil { return true }
        return AppDefaults.current.bool(forKey: enabledKey)
    }

    static func revision() -> UInt64 {
        let existing = (AppDefaults.current.object(forKey: revisionKey) as? NSNumber)?.uint64Value ?? 0
        if existing > 0 { return existing }
        let initial = UInt64(max(1, Int64(Date().timeIntervalSince1970 * 1_000)))
        AppDefaults.current.set(NSNumber(value: initial), forKey: revisionKey)
        return initial
    }

    @discardableResult static func setEnabled(_ enabled: Bool) -> UInt64 {
        if isEnabled() == enabled { return revision() }
        let now = UInt64(max(1, Int64(Date().timeIntervalSince1970 * 1_000)))
        let next = max(revision() + 1, now)
        AppDefaults.current.set(enabled, forKey: enabledKey)
        AppDefaults.current.set(NSNumber(value: next), forKey: revisionKey)
        return next
    }

    static func nextDirectoryRevision() -> UInt64 {
        let existing = (AppDefaults.current.object(forKey: directoryRevisionKey) as? NSNumber)?.uint64Value ?? 0
        let now = UInt64(max(1, Int64(Date().timeIntervalSince1970 * 1_000)))
        let next = max(existing + 1, now)
        AppDefaults.current.set(NSNumber(value: next), forKey: directoryRevisionKey)
        return next
    }
}
