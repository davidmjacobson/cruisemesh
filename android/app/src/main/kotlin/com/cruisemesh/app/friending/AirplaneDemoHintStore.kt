package com.cruisemesh.app.friending

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

private const val PREFS_NAME = "cruisemesh_hints"
private const val PREF_AIRPLANE_DEMO_SHOWN = "airplane_demo_shown"

/**
 * Remembers whether the "it keeps working with no internet" hint has been read.
 *
 * The hint earns its place once, from the first friend onward: that is the
 * first moment somebody has a person to try it with, and the claim is the one
 * people do not take on trust until they have watched it happen. Shown a second
 * time it would be noise, so one flag ends it for good.
 *
 * The flag is written when the person dismisses the hint, not when it is drawn.
 * The hint has two places to appear -- the friend-added sheet, and the chat list
 * underneath it -- and a sheet swiped away in a second would otherwise spend the
 * single showing on a card nobody read, leaving the durable surface with nothing
 * left to display.
 *
 * Exposed as a [StateFlow] so both surfaces react to the same dismissal without
 * either of them polling.
 */
object AirplaneDemoHintStore {

    private val _showHint = MutableStateFlow(false)

    /** True while the hint is still owed to this person. */
    val showHint: StateFlow<Boolean> = _showHint.asStateFlow()

    /** Load the persisted answer into the flow; call once on startup. */
    fun refresh(context: Context) {
        _showHint.value = shouldShow(context)
    }

    fun shouldShow(context: Context): Boolean =
        !context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(PREF_AIRPLANE_DEMO_SHOWN, false)

    /** The person acknowledged the hint; never show it again. */
    fun dismiss(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(PREF_AIRPLANE_DEMO_SHOWN, true)
            .apply()
        _showHint.value = false
    }
}
