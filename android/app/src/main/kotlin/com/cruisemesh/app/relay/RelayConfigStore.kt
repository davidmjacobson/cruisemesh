package com.cruisemesh.app.relay

import android.content.Context

private const val PREFS_NAME = "cruisemesh_relay"
private const val PREF_RELAY_URL = "relay_url"
private const val PREF_RELAY_TOKEN = "relay_token"

data class RelayConfig(
    val relayUrl: String,
    val relayToken: String,
)

/** Persists the optional family relay configuration used for QR sharing and fallback sync. */
object RelayConfigStore {

    fun load(context: Context): RelayConfig? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val relayUrl = prefs.getString(PREF_RELAY_URL, null)?.trim().orEmpty()
        val relayToken = prefs.getString(PREF_RELAY_TOKEN, null)?.trim().orEmpty()
        if (relayUrl.isEmpty() || relayToken.isEmpty()) return null
        return RelayConfig(relayUrl, relayToken)
    }

    fun save(context: Context, relayUrl: String, relayToken: String) {
        val normalizedUrl = relayUrl.trim()
        val normalizedToken = relayToken.trim()
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
        if (normalizedUrl.isEmpty() || normalizedToken.isEmpty()) {
            prefs.remove(PREF_RELAY_URL).remove(PREF_RELAY_TOKEN).apply()
            return
        }
        prefs.putString(PREF_RELAY_URL, normalizedUrl)
            .putString(PREF_RELAY_TOKEN, normalizedToken)
            .apply()
    }
}
