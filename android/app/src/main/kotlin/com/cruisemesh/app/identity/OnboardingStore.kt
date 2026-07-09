package com.cruisemesh.app.identity

import android.content.Context

private const val PREFS_NAME = "cruisemesh_onboarding"
private const val PREF_COMPLETED = "completed"

/** Persists whether the first-run onboarding flow has already been completed. */
object OnboardingStore {

    fun isCompleted(context: Context): Boolean {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        if (prefs.contains(PREF_COMPLETED)) {
            return prefs.getBoolean(PREF_COMPLETED, false)
        }
        // Legacy installs already have a message store on disk; do not block
        // them behind onboarding after an app update.
        return context.applicationContext.filesDir.resolve("cruisemesh.sqlite").exists()
    }

    fun markCompleted(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(PREF_COMPLETED, true)
            .apply()
    }
}
