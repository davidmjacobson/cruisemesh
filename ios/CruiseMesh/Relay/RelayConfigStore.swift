import Foundation

struct RelayConfig: Equatable {
    var relayUrl: String
    var relayToken: String
}

enum RelayConfigStore {
    private static let urlKey = "cruisemesh.relay.url"
    private static let tokenKey = "cruisemesh.relay.token"

    static func load() -> RelayConfig? {
        let url = (UserDefaults.standard.string(forKey: urlKey) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let token = (UserDefaults.standard.string(forKey: tokenKey) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty, !token.isEmpty else { return nil }
        return RelayConfig(relayUrl: url, relayToken: token)
    }

    static func save(relayUrl: String, relayToken: String) {
        let url = relayUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        let token = relayToken.trimmingCharacters(in: .whitespacesAndNewlines)
        if url.isEmpty || token.isEmpty {
            UserDefaults.standard.removeObject(forKey: urlKey)
            UserDefaults.standard.removeObject(forKey: tokenKey)
            return
        }
        UserDefaults.standard.set(url, forKey: urlKey)
        UserDefaults.standard.set(token, forKey: tokenKey)
    }
}
