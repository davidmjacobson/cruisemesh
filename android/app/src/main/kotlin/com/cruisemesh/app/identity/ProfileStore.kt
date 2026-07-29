package com.cruisemesh.app.identity

import android.content.Context

private const val PREFS_NAME = "cruisemesh_profile"
private const val PREF_DISPLAY_NAME = "display_name"
private const val PREF_OWN_AVATAR_EPOCH = "own_avatar_epoch"

/** Persists the local display name used in our QR friend card and friend requests. */
object ProfileStore {

    fun loadDisplayName(context: Context): String =
        loadStoredDisplayName(context).ifEmpty { defaultDisplayName() }

    /**
     * The name the user actually chose, or empty when they have not chosen one.
     *
     * Onboarding prefills from this rather than [loadDisplayName] so the field
     * starts genuinely blank: a slot pre-filled with a plausible-looking value
     * reads as already answered and gets tapped past.
     */
    fun loadStoredDisplayName(context: Context): String =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getString(PREF_DISPLAY_NAME, null)
            ?.trim()
            .orEmpty()

    fun saveDisplayName(context: Context, displayName: String) {
        val normalized = displayName.trim()
        val edit = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
        if (normalized.isEmpty()) {
            edit.remove(PREF_DISPLAY_NAME).apply()
            return
        }
        edit.putString(PREF_DISPLAY_NAME, normalized).apply()
    }

    fun loadOwnAvatarEpoch(context: Context): Long {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        return prefs.getLong(PREF_OWN_AVATAR_EPOCH, 0L)
    }

    fun bumpOwnAvatarEpoch(context: Context): Long {
        val epoch = System.currentTimeMillis()
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putLong(PREF_OWN_AVATAR_EPOCH, epoch)
            .apply()
        return epoch
    }

    /** Reinstalls the profile-photo revision carried by an authenticated backup. */
    fun restoreOwnAvatarEpoch(context: Context, epoch: Long) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putLong(PREF_OWN_AVATAR_EPOCH, epoch.coerceAtLeast(0L))
            .apply()
    }

    /**
     * Last-resort placeholder only -- onboarding requires a real name, so this
     * is reached only by a profile that predates that requirement or was
     * restored without one. Deliberately NOT `Build.MODEL`: naming people after
     * their hardware made every tester on the same phone indistinguishable in
     * each other's contact lists, and on iOS the equivalent call returns a bare
     * "iPhone" for everyone.
     */
    fun defaultDisplayName(): String = "CruiseMesh user"
}
