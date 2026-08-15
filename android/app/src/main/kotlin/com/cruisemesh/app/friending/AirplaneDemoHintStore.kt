package com.cruisemesh.app.friending

import android.content.Context

private const val PREFS_NAME = "cruisemesh_hints"
private const val PREF_AIRPLANE_DEMO_SHOWN = "airplane_demo_shown"

/**
 * Remembers whether the "try airplane mode" hint has already been offered.
 *
 * The hint only earns its place once, at the moment the first friend is added:
 * that is the only point where someone has a person to try it with and has not
 * yet seen the app work without a network. Shown a second time it would be
 * noise, so the flag is written the moment the hint is rendered rather than
 * when it is dismissed -- a person who swipes the sheet away has still seen it.
 */
object AirplaneDemoHintStore {

    fun shouldShow(context: Context): Boolean =
        !context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(PREF_AIRPLANE_DEMO_SHOWN, false)

    fun markShown(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(PREF_AIRPLANE_DEMO_SHOWN, true)
            .apply()
    }
}
