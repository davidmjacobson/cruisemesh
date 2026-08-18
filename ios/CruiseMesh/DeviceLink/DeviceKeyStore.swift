import Foundation
import Security

/// This install's own device keys (`specs/multi-device-v1.md` §3), stored the way
/// `IdentityStore` stores the person's: in the iOS Keychain, which is the
/// platform secret store (DESIGN.md §6.2).
///
/// Two keys, one purpose. §3 splits the deployed Ed25519 identity into a *person
/// root* — whose secret, after migration, exists only inside the passphrase-
/// encrypted `.cmbak` (§14.2) — and a *device* signing key that this phone holds
/// and uses for roster signatures, the §9.3 device offer, and the §9.4 activation
/// acknowledgement. The person root is not here, deliberately: a thief holding
/// this phone must not be able to revoke the person's real devices.
///
/// The blob layout is the core's (`coreEncodeDeviceKeypair`) so that Android, iOS
/// and the desktop store the identical bytes rather than each inventing a layout.
/// Mirrors Android's `DeviceKeyStore`.
enum DeviceKeyStore {
    private static let service = "com.cruisemesh.app.deviceKey"
    private static var account: String {
        "device-keys" + (UITestConfiguration.identityAccountSuffix ?? "")
    }

    /// The keys this device signs with, minted on first use.
    ///
    /// Minting is idempotent per install and never per ceremony: a device that
    /// re-keyed itself between the offer and the acknowledgement would hold a
    /// certificate for a key it had already thrown away (DL-4 — re-linking the
    /// same hardware is what mints a fresh key, and that is a deliberate act).
    static func loadOrCreate() -> DeviceKeypair {
        if let existing = load() { return existing }
        let device = generateDeviceKeypair()
        save(device)
        return device
    }

    /// The stored keys, or nil on an install that has never needed any.
    static func load() -> DeviceKeypair? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else { return nil }
        guard let device = try? coreDecodeDeviceKeypair(bytes: data) else {
            // Same reasoning as IdentityStore's: nothing can recover keys that
            // no longer decode, so drop the stale blob rather than fail every
            // launch. A device that loses its keys has to be linked again, which
            // mints fresh ones (DL-4).
            NSLog("Discarding corrupt stored device keys")
            clear()
            return nil
        }
        return device
    }

    private static func save(_ device: DeviceKeypair) {
        guard let data = try? coreEncodeDeviceKeypair(device: device) else { return }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
    }

    private static func clear() {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
