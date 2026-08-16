package com.cruisemesh.app.ui

import android.content.Context
import com.cruisemesh.app.debug.DebugFileLog

/** Taps on the version row it takes to turn developer settings on, or off again. */
const val DEVELOPER_SETTINGS_UNLOCK_TAPS = 7

/**
 * The tap after which the counter starts saying how many are left.
 *
 * Nothing is said for the first three, so an accidental double-tap on the
 * version line stays invisible.
 */
private const val DEVELOPER_SETTINGS_COUNTDOWN_FROM = 4

/** Taps further apart than this start the count over. */
const val DEVELOPER_SETTINGS_TAP_WINDOW_MS = 3_000L

/**
 * How long the version row keeps showing tap feedback after the last tap.
 *
 * Every bit of feedback this flow gives is the row's own text swapped in place,
 * so this is also how long the row stops reading as a version string. Shorter
 * than [DEVELOPER_SETTINGS_TAP_WINDOW_MS] on purpose: a run that is still live can
 * go quiet, which is the harmless direction. The other way round would leave a
 * stale count on screen after the run it belonged to had already expired.
 */
const val DEVELOPER_SETTINGS_LABEL_REVERT_MS = 1_500L

/** What one tap on the version row means. */
sealed class DeveloperSettingsTap {
    /** Too early to say anything. */
    object Quiet : DeveloperSettingsTap()

    /** Far enough in to be deliberate: [remaining] taps still to go. */
    data class Countdown(val remaining: Int) : DeveloperSettingsTap()

    /** The full run landed. The caller flips the flag. */
    object Reached : DeveloperSettingsTap()
}

/**
 * The seven-tap run on the app-version row, as a plain counter.
 *
 * Pure on purpose: it holds no Context and reads no clock of its own, so the
 * whole rule -- how many taps, how far apart they may be, when the countdown
 * starts -- is exercised by a JVM test instead of by tapping a phone seven
 * times. The caller owns the clock and the persisted flag.
 *
 * A run that stalls expires rather than resuming: taps more than [windowMs]
 * apart start over, so a phone left in a pocket cannot accumulate a run across
 * an afternoon of stray touches. A clock that jumps backwards also starts over,
 * which is the safe direction -- the worst it costs is one more deliberate run.
 */
class DeveloperSettingsTapCounter(
    private val requiredTaps: Int = DEVELOPER_SETTINGS_UNLOCK_TAPS,
    private val windowMs: Long = DEVELOPER_SETTINGS_TAP_WINDOW_MS,
) {
    private var taps = 0
    private var lastTapMs = 0L
    private var started = false

    fun tap(nowMs: Long): DeveloperSettingsTap {
        val continues = started && nowMs >= lastTapMs && nowMs - lastTapMs <= windowMs
        taps = if (continues) taps + 1 else 1
        lastTapMs = nowMs
        started = true
        if (taps >= requiredTaps) {
            reset()
            return DeveloperSettingsTap.Reached
        }
        val remaining = requiredTaps - taps
        return if (taps >= DEVELOPER_SETTINGS_COUNTDOWN_FROM) {
            DeveloperSettingsTap.Countdown(remaining)
        } else {
            DeveloperSettingsTap.Quiet
        }
    }

    fun reset() {
        taps = 0
        lastTapMs = 0L
        started = false
    }
}

/** What the version row reads right now. */
sealed class DeveloperSettingsLabel {
    /** The version string itself: the row's ordinary, resting text. */
    object Version : DeveloperSettingsLabel()

    /** [remaining] taps still to go. */
    data class Countdown(val remaining: Int) : DeveloperSettingsLabel()

    /** The run landed and developer settings are now on. */
    object Unlocked : DeveloperSettingsLabel()

    /** The run landed and developer settings are hidden again. */
    object Hidden : DeveloperSettingsLabel()
}

/**
 * What the version row should read after one tap.
 *
 * The whole of this flow's feedback, as a pure function, because the shape of
 * it is the fix: nothing is drawn over the row and nothing is queued. The row
 * says what happened, in its own place, at its own size, and reverts
 * [DEVELOPER_SETTINGS_LABEL_REVERT_MS] after the last tap. Anything floating above
 * the bottom of the screen -- a toast, a snackbar -- lands exactly on top of
 * the row being tapped and swallows the next tap.
 *
 * [unlockedAfterTap] is the state the flag was left in, so the caller flips the
 * flag and then asks what to say about it.
 */
fun developerSettingsLabelFor(tap: DeveloperSettingsTap, unlockedAfterTap: Boolean): DeveloperSettingsLabel =
    when (tap) {
        DeveloperSettingsTap.Quiet -> DeveloperSettingsLabel.Version
        is DeveloperSettingsTap.Countdown -> DeveloperSettingsLabel.Countdown(tap.remaining)
        DeveloperSettingsTap.Reached ->
            if (unlockedAfterTap) DeveloperSettingsLabel.Unlocked else DeveloperSettingsLabel.Hidden
    }

/**
 * Whether this phone has had developer settings switched on by hand.
 *
 * Persisted, because the reason it exists is a closed-test tester on a
 * release-signed build who needs the engine switches to survive the app being
 * killed between the two halves of a staged-rollout canary run. Its own
 * preferences file, holding one boolean, so deleting the whole mechanism later
 * is deleting a file nothing else reads.
 */
object DeveloperSettingsUnlockStore {
    // Both names predate the rename to "Developer settings" and are kept
    // verbatim: they are what is already on testers' phones, and changing
    // either would silently re-lock every phone mid-canary.
    private const val PREFS_NAME = "cruisemesh_internal_tools"
    private const val PREF_UNLOCKED = "unlocked"

    fun isUnlocked(context: Context): Boolean =
        prefs(context).getBoolean(PREF_UNLOCKED, false)

    fun setUnlocked(context: Context, unlocked: Boolean) {
        prefs(context).edit().putBoolean(PREF_UNLOCKED, unlocked).apply()
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
}

/**
 * Whether the Settings entry for developer settings is shown.
 *
 * Debuggable builds show it unconditionally, exactly as before; a release build
 * shows it once someone has done the seven-tap run.
 */
fun developerSettingsVisible(context: Context): Boolean =
    DebugFileLog.isDebuggableBuild(context) || DeveloperSettingsUnlockStore.isUnlocked(context)

/**
 * Whether to warn inside the screen.
 *
 * True only on a release build that was unlocked by hand -- the case where
 * someone who is not a developer is looking at switches that change how their
 * own messages are delivered.
 */
fun developerSettingsUnlockedOnRelease(context: Context): Boolean =
    !DebugFileLog.isDebuggableBuild(context) && DeveloperSettingsUnlockStore.isUnlocked(context)
