package com.cruisemesh.app.friending

import android.content.Context

/** Local policy advertised through profile-sync v2. */
object FriendsOfFriendsStore {
    private const val PREFS = "friends_of_friends"
    private const val KEY_ENABLED = "enabled"
    private const val KEY_REVISION = "revision"
    private const val KEY_DIRECTORY_REVISION = "directory_revision"

    fun isEnabled(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getBoolean(KEY_ENABLED, true)

    fun revision(context: Context): ULong {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val existing = prefs.getLong(KEY_REVISION, 0L)
        if (existing > 0L) return existing.toULong()
        val initial = System.currentTimeMillis().coerceAtLeast(1L)
        prefs.edit().putLong(KEY_REVISION, initial).apply()
        return initial.toULong()
    }

    fun setEnabled(context: Context, enabled: Boolean): ULong {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (prefs.getBoolean(KEY_ENABLED, true) == enabled) return revision(context)
        val next = maxOf(
            prefs.getLong(KEY_REVISION, 0L) + 1L,
            System.currentTimeMillis().coerceAtLeast(1L),
        )
        prefs.edit()
            .putBoolean(KEY_ENABLED, enabled)
            .putLong(KEY_REVISION, next)
            .apply()
        return next.toULong()
    }

    fun nextDirectoryRevision(context: Context): ULong {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val next = maxOf(
            prefs.getLong(KEY_DIRECTORY_REVISION, 0L) + 1L,
            System.currentTimeMillis().coerceAtLeast(1L),
        )
        prefs.edit().putLong(KEY_DIRECTORY_REVISION, next).apply()
        return next.toULong()
    }
}
