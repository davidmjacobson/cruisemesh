import Foundation

/// Taps on the version row it takes to turn internal tools on, or off again.
let internalToolsUnlockTaps = 7

/// The tap after which the counter starts giving feedback. Nothing happens for
/// the first three, so an accidental double-tap on the version line stays
/// invisible.
private let internalToolsCountdownFrom = 4

/// Taps further apart than this start the count over.
let internalToolsTapWindow: TimeInterval = 3

/// What one tap on the version row means.
enum InternalToolsTap: Equatable {
    /// Too early to say anything.
    case quiet
    /// Far enough in to be deliberate: this many taps still to go.
    case countdown(remaining: Int)
    /// The full run landed. The caller flips the flag.
    case reached
}

/// The seven-tap run on the app-version row, as a plain counter.
///
/// Pure on purpose: it holds no view and reads no clock of its own, so the whole
/// rule -- how many taps, how far apart they may be, when the countdown starts
/// -- is exercised by a test instead of by tapping a phone seven times. Mirrors
/// Android's `InternalToolsTapCounter` exactly, including that a stalled run
/// expires rather than resuming and that a clock jumping backwards starts over.
final class InternalToolsTapCounter {
    private let requiredTaps: Int
    private let window: TimeInterval
    private var taps = 0
    private var lastTap: TimeInterval?

    init(requiredTaps: Int = internalToolsUnlockTaps, window: TimeInterval = internalToolsTapWindow) {
        self.requiredTaps = requiredTaps
        self.window = window
    }

    func tap(at now: TimeInterval) -> InternalToolsTap {
        let continues = lastTap.map { now >= $0 && now - $0 <= window } ?? false
        taps = continues ? taps + 1 : 1
        lastTap = now
        if taps >= requiredTaps {
            reset()
            return .reached
        }
        if taps >= internalToolsCountdownFrom {
            return .countdown(remaining: requiredTaps - taps)
        }
        return .quiet
    }

    func reset() {
        taps = 0
        lastTap = nil
    }
}

/// Whether this phone has had internal tools switched on by hand.
///
/// Persisted, because the reason it exists is a TestFlight tester on a release
/// build who needs the engine switches to survive the app being killed between
/// the two halves of a staged-rollout canary run.
enum InternalToolsUnlockStore {
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
var internalToolsDebugBuild: Bool {
#if DEBUG
    return true
#else
    return false
#endif
}

/// Whether the Settings entry for internal tools is shown. Debug builds show it
/// outright, as they always have; a release build shows it once someone has done
/// the seven-tap run.
var internalToolsVisible: Bool {
    internalToolsDebugBuild || InternalToolsUnlockStore.isUnlocked()
}

/// Whether to warn inside the screen: only on a release build that was unlocked
/// by hand, where someone who is not a developer is looking at switches that
/// change how their own messages are delivered.
var internalToolsUnlockedOnRelease: Bool {
    !internalToolsDebugBuild && InternalToolsUnlockStore.isUnlocked()
}
