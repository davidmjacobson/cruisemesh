package com.cruisemesh.app.ui

import android.content.Context

enum class AppearancePreference(val storageValue: String) {
    SYSTEM("system"),
    LIGHT("light"),
    DARK("dark");

    internal fun resolvesDark(systemIsDark: Boolean): Boolean = when (this) {
        SYSTEM -> systemIsDark
        LIGHT -> false
        DARK -> true
    }

    companion object {
        internal fun fromStoredValue(value: String?): AppearancePreference =
            entries.firstOrNull { it.storageValue == value } ?: SYSTEM
    }
}

object AppearancePreferences {
    private const val PREFERENCES_NAME = "cruisemesh_appearance"
    private const val THEME_KEY = "theme"

    fun load(context: Context): AppearancePreference = AppearancePreference.fromStoredValue(
        context.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE)
            .getString(THEME_KEY, null),
    )

    fun save(context: Context, preference: AppearancePreference) {
        context.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString(THEME_KEY, preference.storageValue)
            .apply()
    }
}
