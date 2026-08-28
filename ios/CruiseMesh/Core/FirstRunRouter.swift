import Foundation

/// The three places first-run setup can send a person.
enum FirstRunDestination: Equatable {
    /// The six-slide wizard, which carries the permissions step inside it.
    case wizard
    /// The permissions step on its own, for a route that arrived past the wizard.
    case permissions
    /// The app.
    case home
}

/// Which screen a launch — or the end of a setup route — should land on.
///
/// There are three ways onto a person's fleet and only one of them is the
/// wizard. "This is another of my devices" and "Restore from backup" both
/// finish by marking setup complete from underneath, which used to drop the
/// phone straight onto the chat list without ever having asked for Bluetooth or
/// notifications. The step is not optional on one route and mandatory on the
/// others, so the decision lives here instead of being spelled three times.
///
/// Plain and framework-free, like `LinkCompletion`, so the rule is pinned by a
/// unit test. Mirrors Android's `FirstRunRouter.kt`.
enum FirstRunRouter {

    /// - Parameters:
    ///   - setupComplete: `OnboardingStore.isCompleted()`.
    ///   - permissionsStepDone: `OnboardingStore.permissionsStepDone()` — `nil`
    ///     for an install older than the flag, which is never sent back through
    ///     setup by an app update.
    ///   - meshPermissionsGranted: whether Bluetooth access is already held.
    ///     Nobody is walked through a step whose only outcome they have already
    ///     produced, however they produced it.
    static func destination(
        setupComplete: Bool,
        permissionsStepDone: Bool?,
        meshPermissionsGranted: Bool
    ) -> FirstRunDestination {
        guard setupComplete else { return .wizard }
        if meshPermissionsGranted { return .home }
        if permissionsStepDone == false { return .permissions }
        return .home
    }
}
