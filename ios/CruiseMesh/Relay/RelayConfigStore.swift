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
        if url.isEmpty || token.isEmpty {
            UserDefaults.standard.removeObject(forKey: configKey)
            UserDefaults.standard.removeObject(forKey: urlKey)
            UserDefaults.standard.removeObject(forKey: tokenKey)
            return
        }
        let config = RelayConfig(relayUrl: url, relayToken: token)
        guard let data = try? JSONEncoder().encode(config) else { return }
        // One value means a process interruption can never leave a URL from
        // one setup paired with a token from another.
        UserDefaults.standard.set(data, forKey: configKey)
        UserDefaults.standard.removeObject(forKey: urlKey)
        UserDefaults.standard.removeObject(forKey: tokenKey)
    }

    static func shareOnline() -> Bool {
        guard UserDefaults.standard.object(forKey: shareOnlineKey) != nil else { return true }
        return UserDefaults.standard.bool(forKey: shareOnlineKey)
    }

    static func setShareOnline(_ enabled: Bool) {
        UserDefaults.standard.set(enabled, forKey: shareOnlineKey)
    }
}
