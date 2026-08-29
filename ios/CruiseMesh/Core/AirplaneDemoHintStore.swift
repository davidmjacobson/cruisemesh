import Foundation

/// Remembers whether the "it keeps working with no internet" hint has been read.
///
/// The hint earns its place once, from the first friend onward: that is the
/// first moment somebody has a person to try it with, and the claim is the one
/// people do not take on trust until they have watched it happen. Shown a
/// second time it would be noise, so one flag ends it for good.
///
/// The flag is written when the person dismisses the hint, not when it is
/// drawn. The hint has two places to appear -- the friend-added sheet, and the
/// chat list underneath it -- and a sheet swiped away in a second would
/// otherwise spend the single showing on a card nobody read, leaving the
/// durable surface with nothing left to display.
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
