import Foundation

enum OnboardingStore {
    private static let completedKey = "cruisemesh.onboarding.completed"

    static func isCompleted() -> Bool {
        AppDefaults.current.bool(forKey: completedKey)
    }

    static func markCompleted() {
        AppDefaults.current.set(true, forKey: completedKey)
    }
}
