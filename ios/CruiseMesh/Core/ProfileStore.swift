import Foundation

enum ProfileStore {
    private static let displayNameKey = "cruisemesh.displayName"

    static func loadDisplayName() -> String {
        UserDefaults.standard.string(forKey: displayNameKey) ?? ""
    }

    static func saveDisplayName(_ name: String) {
        UserDefaults.standard.set(name.trimmingCharacters(in: .whitespacesAndNewlines), forKey: displayNameKey)
    }
}
