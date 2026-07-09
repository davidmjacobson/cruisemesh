package com.cruisemesh.app.identity

import android.content.Context
import android.os.Build

private const val PREFS_NAME = "cruisemesh_profile"
private const val PREF_DISPLAY_NAME = "display_name"

/** Persists the local display name used in our QR friend card and friend requests. */
object ProfileStore {

    fun loadDisplayName(context: Context): String {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        return prefs.getString(PREF_DISPLAY_NAME, null)?.trim().takeUnless { it.isNullOrEmpty() }
            ?: defaultDisplayName()
    }

    fun saveDisplayName(context: Context, displayName: String) {
        val normalized = displayName.trim()
        val edit = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
        if (normalized.isEmpty()) {
            edit.remove(PREF_DISPLAY_NAME).apply()
            return
        }
        edit.putString(PREF_DISPLAY_NAME, normalized).apply()
    }

    fun defaultDisplayName(): String =
        Build.MODEL?.trim().takeUnless { it.isNullOrEmpty() } ?: "CruiseMesh user"
}
