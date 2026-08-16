package com.cruisemesh.app.sail

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Whether the home-screen "before you sail" card has been waved away.
 *
 * Presentation only, which is why it lives here and not in the core policy:
 * the checklist itself is never dismissed, and Settings always has a row to
 * it. This is just the card's own visibility, and it stays put once set --
 * someone who has said "not now" should not be asked again on the next launch.
 *
 * A [StateFlow] rather than a plain read so the card can vanish on the tap
 * that dismissed it, in the same shape as [com.cruisemesh.app.mesh.WifiTipStore].
 */
object SailChecklistCardStore {
    private const val PREFS = "cruisemesh_sail_card"
    private const val KEY_DISMISSED = "card_dismissed"

    private val _dismissed = MutableStateFlow(false)

    /** True once the user has dismissed the home-screen card. */
    val dismissed: StateFlow<Boolean> = _dismissed.asStateFlow()

    /** Load the persisted state into the flow; call when the home screen opens. */
    fun refresh(context: Context) {
        _dismissed.value = prefs(context).getBoolean(KEY_DISMISSED, false)
    }

    /** The user waved the card away; don't offer it again. */
    fun dismiss(context: Context) {
        prefs(context).edit().putBoolean(KEY_DISMISSED, true).apply()
        _dismissed.value = true
    }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
