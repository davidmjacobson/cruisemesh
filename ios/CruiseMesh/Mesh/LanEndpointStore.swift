import Foundation

private struct CachedLanEndpoint: Codable {
    let endpoint: LanManualEndpoint
    let savedAtMs: Int64
}

enum LanEndpointCache {
    private static let prefix = "cruisemesh.lan.endpoint."

    static func save(
        networkId: String?,
        userId: Data,
        endpoint: LanManualEndpoint,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) {
        guard let networkId,
              let encoded = try? JSONEncoder().encode(CachedLanEndpoint(endpoint: endpoint, savedAtMs: nowMs)) else {
            return
        }
        UserDefaults.standard.set(encoded, forKey: key(networkId: networkId, userId: userId))
    }

    static func load(
        networkId: String?,
        userId: Data,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) -> LanManualEndpoint? {
        guard let networkId else { return nil }
        let storageKey = key(networkId: networkId, userId: userId)
        guard let data = UserDefaults.standard.data(forKey: storageKey),
              let cached = try? JSONDecoder().decode(CachedLanEndpoint.self, from: data) else {
            return nil
        }
        guard lanEndpointCacheIsFresh(savedAtMs: cached.savedAtMs, nowMs: nowMs) else {
            UserDefaults.standard.removeObject(forKey: storageKey)
            return nil
        }
        return cached.endpoint
    }

    private static func key(networkId: String, userId: Data) -> String {
        "\(prefix)\(networkId).\(UserIdHex.encode(userId))"
    }
}

enum LanCapabilityStore {
    private static let supportedPrefix = "cruisemesh.lan.supported."
    private static let sentPrefix = "cruisemesh.lan.sent."

    static func markSupported(userId: Data) {
        UserDefaults.standard.set(true, forKey: supportedPrefix + UserIdHex.encode(userId))
    }

    static func isSupported(userId: Data) -> Bool {
        UserDefaults.standard.bool(forKey: supportedPrefix + UserIdHex.encode(userId))
    }

    static func shouldSendEndpoint(
        userId: Data,
        networkId: String,
        endpoint: LanManualEndpoint,
        instanceToken: Data,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) -> Bool {
        let key = sentPrefix + UserIdHex.encode(userId)
        let signature = "\(networkId)|\(endpoint.host)|\(endpoint.port)|\(instanceToken.base64EncodedString())"
        let previous = UserDefaults.standard.dictionary(forKey: key)
        let previousSignature = previous?["signature"] as? String
        let sentAt = (previous?["sentAt"] as? NSNumber)?.int64Value
        if !shouldResendLanEndpoint(
            previousSignature: previousSignature,
            previousSentAtMs: sentAt,
            currentSignature: signature,
            nowMs: nowMs
        ) {
            return false
        }
        UserDefaults.standard.set(
            ["signature": signature, "sentAt": nowMs],
            forKey: key
        )
        return true
    }
}
