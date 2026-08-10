import Foundation

enum TermsAcceptanceStore {
    static let currentVersion = "2026-08-08"
    static let termsURL = URL(string: "https://cruisemesh.app/terms/")!
    static let privacyURL = URL(string: "https://cruisemesh.app/privacy/")!

    private static let acceptedVersionKey = "cruisemesh.terms.acceptedVersion"

    static func isCurrentTermsVersion(_ version: String?) -> Bool {
        version == currentVersion
    }

    static func isCurrentVersionAccepted(defaults: UserDefaults = AppDefaults.current) -> Bool {
        isCurrentTermsVersion(defaults.string(forKey: acceptedVersionKey))
    }

    static func acceptCurrentVersion(defaults: UserDefaults = AppDefaults.current) {
        defaults.set(currentVersion, forKey: acceptedVersionKey)
    }
}
