import Foundation

/// Taps on the version row it takes to turn developer settings on, or off again.
let developerSettingsUnlockTaps = 7

/// The tap after which the counter starts giving feedback. Nothing happens for
/// the first three, so an accidental double-tap on the version line stays
/// invisible.
private let developerSettingsCountdownFrom = 4

/// Taps further apart than this start the count over.
let developerSettingsTapWindow: TimeInterval = 3

/// How long the version row keeps showing tap feedback after the last tap.
///
/// Every bit of feedback this flow gives is the row's own text swapped in
/// place, so this is also how long the row stops reading as a version string.
/// Shorter than `developerSettingsTapWindow` on purpose: a run that is still live
/// can go quiet, which is the harmless direction. The other way round would
/// leave a stale count on screen after the run it belonged to had expired.
let developerSettingsLabelRevert: TimeInterval = 1.5

/// What one tap on the version row means.
enum DeveloperSettingsTap: Equatable {
    /// Too early to say anything.
    case quiet
    /// Far enough in to be deliberate: this many taps still to go.
    case countdown(remaining: Int)
    /// The full run landed. The caller flips the flag.
    case reached
}

/// What the version row reads right now.
enum DeveloperSettingsLabel: Equatable {
    /// The version string itself: the row's ordinary, resting text.
    case version
    /// This many taps still to go.
    case countdown(remaining: Int)
    /// The run landed and developer settings are now on.
    case unlocked
    /// The run landed and developer settings are hidden again.
    case hidden
}

/// What the version row should read after one tap.
///
/// The whole of this flow's feedback, as a pure function, because the shape of
/// it is the point: nothing is drawn over the row and nothing below it moves.
/// The row says what happened, in its own place, at its own size, and reverts
/// `developerSettingsLabelRevert` after the last tap.
///
/// `unlockedAfterTap` is the state the flag was left in, so the caller flips
/// the flag and then asks what to say about it. Mirrors Android's
/// `developerSettingsLabelFor`.
func developerSettingsLabel(for tap: DeveloperSettingsTap, unlockedAfterTap: Bool) -> DeveloperSettingsLabel {
    switch tap {
    case .quiet:
        return .version
    case .countdown(let remaining):
        return .countdown(remaining: remaining)
    case .reached:
        return unlockedAfterTap ? .unlocked : .hidden
    }
}

/// The seven-tap run on the app-version row, as a plain counter.
///
/// Pure on purpose: it holds no view and reads no clock of its own, so the whole
/// rule -- how many taps, how far apart they may be, when the countdown starts
/// -- is exercised by a test instead of by tapping a phone seven times. Mirrors
/// Android's `DeveloperSettingsTapCounter` exactly, including that a stalled run
/// expires rather than resuming and that a clock jumping backwards starts over.
final class DeveloperSettingsTapCounter {
    private let requiredTaps: Int
    private let window: TimeInterval
    private var taps = 0
    private var lastTap: TimeInterval?

    init(requiredTaps: Int = developerSettingsUnlockTaps, window: TimeInterval = developerSettingsTapWindow) {
        self.requiredTaps = requiredTaps
        self.window = window
    }

    func tap(at now: TimeInterval) -> DeveloperSettingsTap {
        let continues = lastTap.map { now >= $0 && now - $0 <= window } ?? false
        taps = continues ? taps + 1 : 1
        lastTap = now
        if taps >= requiredTaps {
            reset()
            return .reached
        }
        if taps >= developerSettingsCountdownFrom {
            return .countdown(remaining: requiredTaps - taps)
        }
        return .quiet
    }

    func reset() {
        taps = 0
        lastTap = nil
    }
}

/// Whether this phone has had developer settings switched on by hand.
///
/// Persisted, because the reason it exists is a TestFlight tester on a release
/// build who needs the engine switches to survive the app being killed between
/// the two halves of a staged-rollout canary run.
enum DeveloperSettingsUnlockStore {
    // The key predates the rename to "Developer settings" and is kept
    // verbatim: it is what is already on testers' phones, and changing it
    // would silently re-lock every phone mid-canary.
    private static let unlockedKey = "cruisemesh.ui.internalToolsUnlocked"

    static func isUnlocked() -> Bool {
        AppDefaults.current.bool(forKey: unlockedKey)
    }

    static func setUnlocked(_ unlocked: Bool) {
        AppDefaults.current.set(unlocked, forKey: unlockedKey)
    }
}

/// True on a build compiled with debugging on -- a developer's own.
///
/// TestFlight and App Store builds are release builds, which is the whole
/// reason the seven-tap run exists.
var developerSettingsDebugBuild: Bool {
#if DEBUG
    return true
#else
    return false
#endif
}

/// Whether the Settings entry for developer settings is shown. Debug builds show it
/// outright, as they always have; a release build shows it once someone has done
/// the seven-tap run.
var developerSettingsVisible: Bool {
    developerSettingsDebugBuild || DeveloperSettingsUnlockStore.isUnlocked()
}

/// Whether to warn inside the screen: only on a release build that was unlocked
/// by hand, where someone who is not a developer is looking at switches that
/// change how their own messages are delivered.
var developerSettingsUnlockedOnRelease: Bool {
    !developerSettingsDebugBuild && DeveloperSettingsUnlockStore.isUnlocked()
}
