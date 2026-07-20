package com.cruisemesh.app.mesh

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * T15 phase 3: persists how many times we've seen the "Wi-Fi dropped out from
 * under a live mesh while cellular stayed up" pattern, plus whether the user
 * has dismissed the resulting tip. Exposes a [StateFlow] so a contextual banner
 * can react without polling. See [WifiDropPolicy] for when the tip qualifies.
 */
object WifiTipStore {
    private const val PREFS = "cruisemesh_wifi_tip"
    private const val KEY_OCCURRENCES = "premature_drop_count"
    private const val KEY_DISMISSED = "tip_dismissed"

    private val _showTip = MutableStateFlow(false)
    /** True when the contextual "keep Wi-Fi on" tip should be visible. */
    val showTip: StateFlow<Boolean> = _showTip.asStateFlow()

    /** Load persisted state into the flow; call once on startup. */
    fun refresh(context: Context) {
        _showTip.value = computeShouldShow(prefs(context))
    }

    /** Record one premature Wi-Fi drop and update the tip visibility. */
    fun recordPrematureDrop(context: Context) {
        val p = prefs(context)
        val next = p.getInt(KEY_OCCURRENCES, 0) + 1
        p.edit().putInt(KEY_OCCURRENCES, next).apply()
        _showTip.value = computeShouldShow(p)
    }

    /** The user acknowledged the tip; don't show it again. */
    fun dismiss(context: Context) {
        prefs(context).edit().putBoolean(KEY_DISMISSED, true).apply()
        _showTip.value = false
    }

    private fun computeShouldShow(p: android.content.SharedPreferences): Boolean =
        !p.getBoolean(KEY_DISMISSED, false) &&
            WifiDropPolicy.shouldShowTip(p.getInt(KEY_OCCURRENCES, 0))

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
