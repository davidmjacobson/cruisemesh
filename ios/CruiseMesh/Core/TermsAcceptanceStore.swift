import Foundation

enum TermsAcceptanceStore {
    static let currentVersion = "2026-07-23"
    static let termsURL = URL(string: "https://cruisemesh.app/terms/")!
    static let privacyURL = URL(string: "https://cruisemesh.app/privacy/")!

    private static let acceptedVersionKey = "cruisemesh.terms.acceptedVersion"

    static func isCurrentVersionAccepted(defaults: UserDefaults = .standard) -> Bool {
        defaults.string(forKey: acceptedVersionKey) == currentVersion
    }

    static func acceptCurrentVersion(defaults: UserDefaults = .standard) {
        defaults.set(currentVersion, forKey: acceptedVersionKey)
    }
}
