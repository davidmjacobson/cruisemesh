import Foundation
import Security

/// §6's person-scoped inbox key, at whatever generation this fleet has reached.
///
/// The core generates it and never persists it (`InboxKey`'s own contract), so
/// somebody has to, and §10.1's two-call ceremony makes *when* load-bearing:
/// `beginOwnRevocation` hands the rotated key out, this file has to make it
/// durable, and only then may `commitOwnRevocation` re-seal the backlog to it. A
/// crash between those two is recoverable exactly because `generation()` can be
/// asked afterwards whether the key survived.
///
/// # Generation 0 is not stored, because it is not a secret of its own
///
/// §10's note 4: "Inbox key generation 0 *is* the deployed person agreement key".
/// Every install in the field already holds it as `Identity.agreeSk` — it is the
/// key on every friend card — so writing a second copy here would be a second
/// place for the same secret to leak from and a second place for a restore to
/// disagree with itself. `keyFor` therefore *derives* generation 0 from the
/// identity and only reads storage above it.
///
/// # Layout
///
/// Three fields, and no invented blob format: the generation and the public half
/// are plain `UserDefaults` values because both are public (the generation rides
/// in every roster; the public key is what siblings seal to), and the bare
/// 32-byte secret is a Keychain item, which is this platform's secret store the
/// same way `IdentityStore` and `DeviceKeyStore` use it. Storing the secret alone
/// rather than an encoded record is what keeps this from being a wire format that
/// Android would have to match byte for byte — Android's `InboxKeyStore` makes
/// exactly the same decomposition into SharedPreferences plus an AndroidKeystore
/// blob, and neither shell can drift into a layout the other has to parse.
///
/// There is no `core_encode_inbox_key`; if one ever lands, both shells should
/// move onto it and this decomposition goes away.
enum InboxKeyStore {
    private static let generationKey = "cruisemesh.inboxKey.generation"
    private static let agreePkKey = "cruisemesh.inboxKey.agreePk"
    private static let service = "com.cruisemesh.app.inboxKey"
    private static var account: String {
        "inbox-key" + (UITestConfiguration.identityAccountSuffix ?? "")
    }

    /// The key for `generation`, or nil if this device does not hold it.
    ///
    /// Generation 0 is answered from `identity` (see the type note); every other
    /// generation must have been written down by `save`.
    static func keyFor(
        identity: Identity,
        generation: UInt64,
        defaults: UserDefaults = AppDefaults.current
    ) -> InboxKey? {
        if generation == 0 {
            return InboxKey(
                generation: 0,
                agreePk: identity.agreePk,
                agreeSk: identity.agreeSk
            )
        }
        guard let stored = load(defaults: defaults), stored.generation == generation else {
            return nil
        }
        return stored
    }

    /// The key this fleet's roster currently names, for a caller that is about to
    /// rotate it. Nil means the roster has climbed to a generation whose key this
    /// device never received — a sibling that has not caught up, and a revocation
    /// it must not attempt.
    static func current(
        identity: Identity,
        inboxKeyGeneration: UInt64,
        defaults: UserDefaults = AppDefaults.current
    ) -> InboxKey? {
        keyFor(identity: identity, generation: inboxKeyGeneration, defaults: defaults)
    }

    /// The stored key, or nil on an install that has never rotated one.
    static func load(defaults: UserDefaults = AppDefaults.current) -> InboxKey? {
        guard defaults.object(forKey: generationKey) != nil else { return nil }
        let generation = UInt64(max(0, defaults.integer(forKey: generationKey)))
        guard let agreePk = defaults.data(forKey: agreePkKey), let secret = loadSecret() else {
            // Unlike a device key, this one must NOT be silently dropped and
            // re-minted: rows are sealed to it, and a fresh key would not open
            // them. Leave everything exactly where it is and answer honestly
            // that this device cannot read it, so the caller declines the
            // rotation instead of performing one it cannot finish.
            NSLog("Stored inbox key is incomplete on this device")
            return nil
        }
        return InboxKey(generation: generation, agreePk: agreePk, agreeSk: secret)
    }

    /// Which generation this device holds, for §10.1's crash-recovery question.
    static func generation(defaults: UserDefaults = AppDefaults.current) -> UInt64? {
        guard defaults.object(forKey: generationKey) != nil else { return nil }
        return UInt64(max(0, defaults.integer(forKey: generationKey)))
    }

    /// Make a rotated key durable. Must return before
    /// `MessageStore.commitOwnRevocation` runs — the whole point of the two-call
    /// ceremony is that the backlog is never re-sealed to a secret that only
    /// exists in memory.
    ///
    /// The secret lands first. A crash between the two writes leaves a Keychain
    /// item nothing points at, which `load` reports as "incomplete" and which the
    /// next rotation overwrites; the other ordering would leave a generation
    /// recorded for a secret that was never stored, which is the one state
    /// `RemoveDeviceSession.repairPending` cannot tell from a survivable crash.
    static func save(_ key: InboxKey, defaults: UserDefaults = AppDefaults.current) {
        saveSecret(key.agreeSk)
        defaults.set(key.agreePk, forKey: agreePkKey)
        defaults.set(Int(clamping: key.generation), forKey: generationKey)
    }

    private static func loadSecret() -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data, !data.isEmpty else { return nil }
        return data
    }

    private static func saveSecret(_ secret: Data) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = secret
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
    }
}
