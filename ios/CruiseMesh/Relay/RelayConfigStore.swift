import Foundation

struct RelayConfig: Codable, Equatable {
    var relayUrl: String
    var relayToken: String
}

func normalizeRelayUrl(_ value: String) -> String {
    normalizeRelayUrl(value: value)
}

enum RelayConfigStore {
    private static let urlKey = "cruisemesh.relay.url"
    private static let tokenKey = "cruisemesh.relay.token"
    private static let configKey = "cruisemesh.relay.config.v1"
    private static let shareOnlineKey = "cruisemesh.relay.shareOnline"
    /// T23: monotonic version of *this device's own* relay endpoint. Bumped
    /// only when `save` actually changes the configuration, and carried in
    /// every relay-change notice so a contact can order notices that arrive
    /// out of sequence (DTN reordering, relay replays).
    private static let relayEpochKey = "cruisemesh.relay.epoch"
    /// T23: the highest epoch already fanned out to every contact.
    private static let announcedRelayEpochKey = "cruisemesh.relay.announcedEpoch"

    static func load() -> RelayConfig? {
        if let data = UserDefaults.standard.data(forKey: configKey),
           let config = try? JSONDecoder().decode(RelayConfig.self, from: data) {
            let url = normalizeRelayUrl(config.relayUrl)
            let token = config.relayToken.trimmingCharacters(in: .whitespacesAndNewlines)
            if !url.isEmpty, !token.isEmpty {
                return RelayConfig(relayUrl: url, relayToken: token)
            }
        }
        // One-time migration from the original two-key representation.
        let url = normalizeRelayUrl(UserDefaults.standard.string(forKey: urlKey) ?? "")
        let token = (UserDefaults.standard.string(forKey: tokenKey) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty, !token.isEmpty else { return nil }
        let config = RelayConfig(relayUrl: url, relayToken: token)
        if let data = try? JSONEncoder().encode(config) {
            UserDefaults.standard.set(data, forKey: configKey)
            UserDefaults.standard.removeObject(forKey: urlKey)
            UserDefaults.standard.removeObject(forKey: tokenKey)
        }
        return config
    }

    static func save(relayUrl: String, relayToken: String) {
        let url = normalizeRelayUrl(relayUrl)
        let token = relayToken.trimmingCharacters(in: .whitespacesAndNewlines)
        let next = (url.isEmpty || token.isEmpty)
            ? nil
            : RelayConfig(relayUrl: url, relayToken: token)
        // T23: only a real change bumps the epoch. Settings screens re-save on
        // every keystroke, and a no-op save must not make contacts re-apply an
        // endpoint they already hold.
        guard next != load() else { return }

        if let next {
            guard let data = try? JSONEncoder().encode(next) else { return }
            // One value means a process interruption can never leave a URL
            // from one setup paired with a token from another.
            UserDefaults.standard.set(data, forKey: configKey)
        } else {
            UserDefaults.standard.removeObject(forKey: configKey)
        }
        UserDefaults.standard.removeObject(forKey: urlKey)
        UserDefaults.standard.removeObject(forKey: tokenKey)
        UserDefaults.standard.set(nextRelayEpoch(), forKey: relayEpochKey)
    }

    /// T23: the current epoch of this device's own relay endpoint. `0` means
    /// it has never changed since install, so there is nothing to announce.
    static func relayEpoch() -> Int64 {
        Int64(UserDefaults.standard.integer(forKey: relayEpochKey))
    }

    /// T23: the newest epoch already fanned out to contacts.
    static func announcedRelayEpoch() -> Int64 {
        Int64(UserDefaults.standard.integer(forKey: announcedRelayEpochKey))
    }

    /// T23: records that `epoch` has been queued to every contact.
    static func markRelayEpochAnnounced(_ epoch: Int64) {
        UserDefaults.standard.set(Int(epoch), forKey: announcedRelayEpochKey)
    }

    /// Wall clock, but never at or below the previous value: a backwards clock
    /// (manual change, NTP correction) must not mint an epoch a contact would
    /// ignore as stale, which would strand them on a dead endpoint forever.
    private static func nextRelayEpoch() -> Int64 {
        max(Int64(Date().timeIntervalSince1970 * 1000), relayEpoch() + 1)
    }

    static func shareOnline() -> Bool {
        guard UserDefaults.standard.object(forKey: shareOnlineKey) != nil else { return true }
        return UserDefaults.standard.bool(forKey: shareOnlineKey)
    }

    static func setShareOnline(_ enabled: Bool) {
        UserDefaults.standard.set(enabled, forKey: shareOnlineKey)
    }
}
