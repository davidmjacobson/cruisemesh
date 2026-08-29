import Foundation

enum OnboardingStore {
    private static let completedKey = "cruisemesh.onboarding.completed"
    private static let permissionsStepKey = "cruisemesh.onboarding.permissionsStepDone"

    static func isCompleted() -> Bool {
        AppDefaults.current.bool(forKey: completedKey)
    }

    static func markCompleted() {
        AppDefaults.current.set(true, forKey: completedKey)
    }

    /// Whether this phone has been walked through the permissions step, or
    /// `nil` when nothing ever recorded an answer.
    ///
    /// Three states rather than two, because the two doors that mark setup
    /// complete without ever showing the step (`markPermissionsStepPending`)
    /// have to be told apart from the installs that predate this flag. A
    /// `false` means "a route skipped the step and owes it"; a missing value
    /// means "this install is older than the question" and must not be pulled
    /// back into first-run setup by an app update.
    ///
    /// Mirrors Android's `OnboardingStore.permissionsStepDone`.
    static func permissionsStepDone() -> Bool? {
        AppDefaults.current.object(forKey: permissionsStepKey) as? Bool
    }

    /// The person has seen the permissions step and moved past it.
    static func markPermissionsStepDone() {
        AppDefaults.current.set(true, forKey: permissionsStepKey)
    }

    /// This phone was set up by a route that never showed the permissions step
    /// — an own-device link or a backup restore — so it still owes one.
    static func markPermissionsStepPending() {
        AppDefaults.current.set(false, forKey: permissionsStepKey)
    }
}
