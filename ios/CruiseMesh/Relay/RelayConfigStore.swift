import Foundation
import os.log

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
        if let data = AppDefaults.current.data(forKey: configKey),
           let config = try? JSONDecoder().decode(RelayConfig.self, from: data) {
            let url = normalizeRelayUrl(config.relayUrl)
            let token = config.relayToken.trimmingCharacters(in: .whitespacesAndNewlines)
            if !url.isEmpty, !token.isEmpty {
                return RelayConfig(relayUrl: url, relayToken: token)
            }
        }
        // One-time migration from the original two-key representation.
        let url = normalizeRelayUrl(AppDefaults.current.string(forKey: urlKey) ?? "")
        let token = (AppDefaults.current.string(forKey: tokenKey) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty, !token.isEmpty else { return nil }
        let config = RelayConfig(relayUrl: url, relayToken: token)
        if let data = try? JSONEncoder().encode(config) {
            AppDefaults.current.set(data, forKey: configKey)
            AppDefaults.current.removeObject(forKey: urlKey)
            AppDefaults.current.removeObject(forKey: tokenKey)
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
            AppDefaults.current.set(data, forKey: configKey)
        } else {
            AppDefaults.current.removeObject(forKey: configKey)
        }
        AppDefaults.current.removeObject(forKey: urlKey)
        AppDefaults.current.removeObject(forKey: tokenKey)
        AppDefaults.current.set(nextRelayEpoch(), forKey: relayEpochKey)
    }

    /// T23: the current epoch of this device's own relay endpoint. `0` means
    /// it has never changed since install, so there is nothing to announce.
    static func relayEpoch() -> Int64 {
        Int64(AppDefaults.current.integer(forKey: relayEpochKey))
    }

    /// T23: the newest epoch already fanned out to contacts.
    static func announcedRelayEpoch() -> Int64 {
        Int64(AppDefaults.current.integer(forKey: announcedRelayEpochKey))
    }

    /// T23: records that `epoch` has been queued to every contact.
    static func markRelayEpochAnnounced(_ epoch: Int64) {
        AppDefaults.current.set(Int(epoch), forKey: announcedRelayEpochKey)
    }

    /// Wall clock, but never at or below the previous value: a backwards clock
    /// (manual change, NTP correction) must not mint an epoch a contact would
    /// ignore as stale, which would strand them on a dead endpoint forever.
    private static func nextRelayEpoch() -> Int64 {
        max(Int64(Date().timeIntervalSince1970 * 1000), relayEpoch() + 1)
    }

    static func shareOnline() -> Bool {
        guard AppDefaults.current.object(forKey: shareOnlineKey) != nil else { return true }
        return AppDefaults.current.bool(forKey: shareOnlineKey)
    }

    static func setShareOnline(_ enabled: Bool) {
        AppDefaults.current.set(enabled, forKey: shareOnlineKey)
    }

    private static let log = Logger(subsystem: "com.cruisemesh", category: "RelayClient")

    /// Records the Shore Pass this device is actually using, once per launch.
    ///
    /// Without it a shared archive cannot answer the first question anyone
    /// asks about a relay problem -- is this phone even configured, and with
    /// which pass? A log full of relay silence looks identical whether the
    /// pass is missing, pointed at a dead host, or working perfectly with
    /// nothing to carry.
    ///
    /// The pass is named by a digest of its token, never by any part of the
    /// token. That is enough to tell one household's pass from another's, and
    /// from the shared tester pass, and to recognise the same pass across two
    /// logs -- which is all triage ever needed from it. A prefix answered the
    /// same question by printing eight characters of a live bearer credential
    /// into a file that gets mailed to whoever is helping.
    ///
    /// Derived in the core (`relayTokenFingerprint`) so this shell and
    /// Android print the same label for the same pass.
    static func logSummary() {
        guard let config = load() else {
            // Deliberately hedged. This also runs on a background Bluetooth
            // relaunch, which after a reboot can happen before first unlock,
            // when the UserDefaults plist may not be readable yet -- `load()`
            // returns nil for a perfectly configured phone. Stating "no Cruise
            // Pass" there would answer the exact triage question this line
            // exists for, wrongly, and send someone chasing a missing pass
            // that is not missing.
            log.info("No relay configuration readable at launch (unset, or device not yet unlocked)")
            return
        }
        let host = URL(string: config.relayUrl)?.host ?? "unparseable"
        log.info(
            """
            Relay configured: host=\(host, privacy: .public) \
            pass=\(tokenFingerprint(config.relayToken), privacy: .public) \
            epoch=\(relayEpoch(), privacy: .public) \
            shareOnline=\(shareOnline(), privacy: .public)
            """
        )
    }

    /// A digest of the pass, never any part of the pass itself. Mirrors
    /// Android `RelayConfigStore.tokenFingerprint`; both go through the core
    /// so the two archives name one pass the same way.
    static func tokenFingerprint(_ token: String) -> String {
        relayTokenFingerprint(relayToken: token)
    }
}
