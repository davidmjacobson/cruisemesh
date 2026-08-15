package com.cruisemesh.app.ui

import android.content.Context
import com.cruisemesh.app.debug.DebugFileLog

/** Taps on the version row it takes to turn internal tools on, or off again. */
const val INTERNAL_TOOLS_UNLOCK_TAPS = 7

/**
 * The tap after which the counter starts saying how many are left.
 *
 * Nothing is said for the first three, so an accidental double-tap on the
 * version line stays invisible.
 */
private const val INTERNAL_TOOLS_COUNTDOWN_FROM = 4

/** Taps further apart than this start the count over. */
const val INTERNAL_TOOLS_TAP_WINDOW_MS = 3_000L

/** What one tap on the version row means. */
sealed class InternalToolsTap {
    /** Too early to say anything. */
    object Quiet : InternalToolsTap()

    /** Far enough in to be deliberate: [remaining] taps still to go. */
    data class Countdown(val remaining: Int) : InternalToolsTap()

    /** The full run landed. The caller flips the flag. */
    object Reached : InternalToolsTap()
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
class InternalToolsTapCounter(
    private val requiredTaps: Int = INTERNAL_TOOLS_UNLOCK_TAPS,
    private val windowMs: Long = INTERNAL_TOOLS_TAP_WINDOW_MS,
) {
    private var taps = 0
    private var lastTapMs = 0L
    private var started = false

    fun tap(nowMs: Long): InternalToolsTap {
        val continues = started && nowMs >= lastTapMs && nowMs - lastTapMs <= windowMs
        taps = if (continues) taps + 1 else 1
        lastTapMs = nowMs
        started = true
        if (taps >= requiredTaps) {
            reset()
            return InternalToolsTap.Reached
        }
        val remaining = requiredTaps - taps
        return if (taps >= INTERNAL_TOOLS_COUNTDOWN_FROM) {
            InternalToolsTap.Countdown(remaining)
        } else {
            InternalToolsTap.Quiet
        }
    }

    fun reset() {
        taps = 0
        lastTapMs = 0L
        started = false
    }
}

/**
 * Whether this phone has had internal tools switched on by hand.
 *
 * Persisted, because the reason it exists is a closed-test tester on a
 * release-signed build who needs the engine switches to survive the app being
 * killed between the two halves of a staged-rollout canary run. Its own
 * preferences file, holding one boolean, so deleting the whole mechanism later
 * is deleting a file nothing else reads.
 */
object InternalToolsUnlockStore {
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
 * Whether the Settings entry for internal tools is shown.
 *
 * Debuggable builds show it unconditionally, exactly as before; a release build
 * shows it once someone has done the seven-tap run.
 */
fun internalToolsVisible(context: Context): Boolean =
    DebugFileLog.isDebuggableBuild(context) || InternalToolsUnlockStore.isUnlocked(context)

/**
 * Whether to warn inside the screen.
 *
 * True only on a release build that was unlocked by hand -- the case where
 * someone who is not a developer is looking at switches that change how their
 * own messages are delivered.
 */
fun internalToolsUnlockedOnRelease(context: Context): Boolean =
    !DebugFileLog.isDebuggableBuild(context) && InternalToolsUnlockStore.isUnlocked(context)
