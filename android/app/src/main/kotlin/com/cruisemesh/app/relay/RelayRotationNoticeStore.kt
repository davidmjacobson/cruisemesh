package com.cruisemesh.app.relay

import android.content.Context

private const val PREFS_NAME = "cruisemesh_relay"
private const val PREF_ROTATION_BLOCKED = "relay_rotation_blocked"

/**
 * One fact, kept for the surface: **the relay refused to re-key this family,
 * for good** (`specs/multi-device-v1.md` §10 step 2).
 *
 * Removing a device promises the removed phone loses the family's Shore Pass
 * mailbox. Nearly always it does — but two answers from the relay mean it never
 * will from this device: a family whose token the operator manages rather than
 * the family (`rotation_unsupported`), and a family whose rotation authority is
 * somebody else's key (`rotation_unauthorized`, which is what every household
 * after the first gets on a *shared* pass). [RelayRotationDriver] stops asking
 * in both cases, correctly — and used to stop silently, which left a person
 * holding a promise the app had privately given up on.
 *
 * So it is written down here instead. Durable rather than per-process, because
 * the refusal outlives the launch it happened on and the person may not open
 * Your devices for a week; cleared the moment a rotation is planned afresh or
 * one lands, because either makes the note untrue.
 *
 * Not an error banner anywhere: this is a state of the family's pass, and the
 * screen where the person made the promise is where they will come looking.
 */
object RelayRotationNoticeStore {

    /**
     * True when a device removed from this person's fleet may still be able to
     * reach the family mailbox because the pass could not be changed.
     */
    fun blocked(context: Context): Boolean =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(PREF_ROTATION_BLOCKED, false)

    internal fun setBlocked(context: Context, blocked: Boolean) {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        // Only a change is written: this runs on every relay pass that settles
        // a rotation, and clearing a flag that is already clear would rewrite
        // the file for nothing.
        if (prefs.getBoolean(PREF_ROTATION_BLOCKED, false) == blocked) return
        prefs.edit().putBoolean(PREF_ROTATION_BLOCKED, blocked).apply()
    }
}
