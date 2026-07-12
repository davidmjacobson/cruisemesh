import Foundation

enum OnboardingStore {
    private static let completedKey = "cruisemesh.onboarding.completed"

    static func isCompleted() -> Bool {
        UserDefaults.standard.bool(forKey: completedKey)
    }

    static func markCompleted() {
        UserDefaults.standard.set(true, forKey: completedKey)
    }
}
