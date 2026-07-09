import Foundation
import Security

/// Persists the UniFFI `Identity` in the iOS Keychain (AES-GCM not needed —
/// Keychain is the platform secret store, DESIGN.md §6.2).
enum IdentityStore {
    private static let service = "com.cruisemesh.app.identity"
    private static let account = "device-identity"
    private static let fieldSizes = [16, 32, 32, 32, 32]

    static func load() -> Identity? {
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
        return decodeIdentity(data)
    }

    static func save(_ identity: Identity) {
        let data = encodeIdentity(identity)
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

    static func loadOrCreate() -> Identity {
        if let existing = load() { return existing }
        let identity = generateIdentity()
        save(identity)
        return identity
    }

    static func encodeIdentity(_ identity: Identity) -> Data {
        var data = Data()
        data.append(contentsOf: identity.userId)
        data.append(contentsOf: identity.signPk)
        data.append(contentsOf: identity.signSk)
        data.append(contentsOf: identity.agreePk)
        data.append(contentsOf: identity.agreeSk)
        return data
    }

    static func decodeIdentity(_ data: Data) -> Identity? {
        let expected = fieldSizes.reduce(0, +)
        guard data.count == expected else { return nil }
        var offset = 0
        func take(_ n: Int) -> Data {
            let slice = data.subdata(in: offset..<offset + n)
            offset += n
            return slice
        }
        return Identity(
            userId: take(16),
            signPk: take(32),
            signSk: take(32),
            agreePk: take(32),
            agreeSk: take(32)
        )
    }
}
