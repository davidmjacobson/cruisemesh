import Foundation

/// Remembers whether the "try airplane mode" hint has already been offered.
///
/// The hint only earns its place once, at the moment the first friend is
/// added: that is the only point where someone has a person to try it with and
/// has not yet seen the app work without a network. Shown a second time it
/// would be noise, so the flag is written as the hint appears rather than when
/// it is dismissed -- someone who swipes the sheet away has still seen it.
///
/// Mirrors Android's `AirplaneDemoHintStore`.
enum AirplaneDemoHintStore {
    private static let shownKey = "cruisemesh.hints.airplaneDemoShown"

    static func shouldShow(defaults: UserDefaults = AppDefaults.current) -> Bool {
        !defaults.bool(forKey: shownKey)
    }

    static func markShown(defaults: UserDefaults = AppDefaults.current) {
        defaults.set(true, forKey: shownKey)
    }
}
