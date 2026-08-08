import Foundation

/// The shape builds before provenance wrote: a JSON blob, with no record of
/// how the address became known. Still decoded so an upgrade keeps whatever
/// those builds learned -- as `LanEndpointProvenance.hinted`, the
/// conservative reading, since those builds filed hints and proven addresses
/// through the same door.
private struct LegacyCachedLanEndpoint: Codable {
    let endpoint: LanManualEndpoint
    let savedAtMs: Int64
}

/// Per-network memory of where a contact was last reachable over Wi-Fi.
///
/// Every entry records how the address became known -- see
/// `LanEndpointProvenance`. The shared core owns the stored format and the
/// rules; this type only reaches `UserDefaults`, so both apps agree byte for
/// byte on what an entry means.
enum LanEndpointCache {
    private static let prefix = "cruisemesh.lan.endpoint."
    /// Serialises the read-modify-write in `save` and the read-then-delete in
    /// `load`. `UserDefaults` is atomic per operation but not across a pair of
    /// them, and a hint that read an entry before a handshake promoted it
    /// would write the promotion away -- the demotion
    /// `lanEndpointCacheEncodeUpdate` exists to prevent. Neither body calls
    /// the other, so a plain lock is enough.
    private static let lock = NSLock()

    static func save(
        networkId: String?,
        userId: Data,
        endpoint: LanManualEndpoint,
        provenance: LanEndpointProvenance,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) {
        guard let networkId, lanEndpointHostIsLocal(host: endpoint.host) else { return }
        let storageKey = key(networkId: networkId, userId: userId)
        let entry = LanEndpointCacheEntry(
            host: endpoint.host,
            port: endpoint.port,
            savedAtMs: nowMs,
            provenance: provenance
        )
        lock.lock()
        defer { lock.unlock() }
        UserDefaults.standard.set(
            lanEndpointCacheEncodeUpdate(existingValue: storedValue(forKey: storageKey), entry: entry),
            forKey: storageKey
        )
    }

    /// The cached endpoint for this contact on this network, if one may still
    /// be dialed. `localHost` is this phone's own LAN address, which is what
    /// lets an unproven entry be checked against the network we are actually
    /// on; an entry the core rules out for good is deleted here rather than
    /// left to age out over seven days.
    static func load(
        networkId: String?,
        userId: Data,
        localHost: String?,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) -> LanManualEndpoint? {
        guard let networkId else { return nil }
        let storageKey = key(networkId: networkId, userId: userId)
        lock.lock()
        defer { lock.unlock() }
        guard let stored = storedValue(forKey: storageKey) else {
            // Nothing readable here. If something is nonetheless stored under
            // the key it is in neither shape and never will be, so it is
            // deleted rather than re-read and re-rejected on every Wi-Fi join
            // -- Android deletes the equivalent unreadable string. Removing an
            // absent key is a no-op, which covers the ordinary empty case.
            UserDefaults.standard.removeObject(forKey: storageKey)
            return nil
        }
        guard let entry = lanEndpointCacheDecode(value: stored) else {
            UserDefaults.standard.removeObject(forKey: storageKey)
            return nil
        }
        switch lanEndpointCacheDecision(entry: entry, localHost: localHost, nowMs: nowMs) {
        case .use:
            return LanManualEndpoint(host: entry.host, port: entry.port)
        case .skip:
            return nil
        case .evict:
            UserDefaults.standard.removeObject(forKey: storageKey)
            return nil
        }
    }

    /// The stored value in the current format, rewriting a legacy JSON blob
    /// into it on the way past so nothing downstream has to know two shapes.
    /// A value that is neither is `nil`, and the caller deletes it.
    private static func storedValue(forKey storageKey: String) -> String? {
        if let value = UserDefaults.standard.string(forKey: storageKey) {
            return value
        }
        guard let data = UserDefaults.standard.data(forKey: storageKey),
              let legacy = try? JSONDecoder().decode(LegacyCachedLanEndpoint.self, from: data) else {
            return nil
        }
        let migrated = lanEndpointCacheEncode(entry: LanEndpointCacheEntry(
            host: legacy.endpoint.host,
            port: legacy.endpoint.port,
            savedAtMs: legacy.savedAtMs,
            provenance: .hinted
        ))
        // The saved-at millisecond is carried across unchanged: a migration is
        // not new evidence and must not restart the seven-day clock.
        UserDefaults.standard.set(migrated, forKey: storageKey)
        return migrated
    }

    private static func key(networkId: String, userId: Data) -> String {
        "\(prefix)\(networkId).\(UserIdHex.encode(userId))"
    }
}

enum LanCapabilityStore {
    private static let supportedPrefix = "cruisemesh.lan.supported."
    private static let lastSeenPrefix = "cruisemesh.lan.supported.seen."
    private static let sentPrefix = "cruisemesh.lan.sent."

    static func markSupported(
        userId: Data,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) {
        let key = UserIdHex.encode(userId)
        UserDefaults.standard.set(true, forKey: supportedPrefix + key)
        UserDefaults.standard.set(NSNumber(value: nowMs), forKey: lastSeenPrefix + key)
    }

    static func isSupported(userId: Data) -> Bool {
        UserDefaults.standard.bool(forKey: supportedPrefix + UserIdHex.encode(userId))
    }

    /// When this contact last demonstrated LAN support, or nil if it never
    /// has -- including contacts marked supported by a build that predates
    /// this timestamp, which stop motivating automatic sweeps until the next
    /// link or endpoint hint records a fresh one. See
    /// `lanCapabilityMotivatesScan`.
    static func lastSupportedAtMs(userId: Data) -> Int64? {
        let stored = UserDefaults.standard.object(forKey: lastSeenPrefix + UserIdHex.encode(userId))
        guard let lastSeen = (stored as? NSNumber)?.int64Value, lastSeen > 0 else { return nil }
        return lastSeen
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
