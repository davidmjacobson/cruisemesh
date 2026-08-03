package com.cruisemesh.app.relay

import android.content.Context
import android.util.Log
import com.cruisemesh.app.persist

private const val PREFS_NAME = "cruisemesh_relay"
private const val PREF_RELAY_URL = "relay_url"
private const val PREF_RELAY_TOKEN = "relay_token"
private const val PREF_SHARE_ONLINE = "share_online"

/**
 * T23: monotonic version of *this device's own* relay endpoint. Bumped only
 * when [RelayConfigStore.save] actually changes the configuration, and carried
 * in every relay-change notice so a contact can order notices that arrive out
 * of sequence (DTN reordering, relay replays).
 */
private const val PREF_RELAY_EPOCH = "relay_epoch"

/** T23: the highest [PREF_RELAY_EPOCH] already fanned out to every contact. */
private const val PREF_ANNOUNCED_RELAY_EPOCH = "relay_announced_epoch"

data class RelayConfig(
    val relayUrl: String,
    val relayToken: String,
)

/** Canonical relay base URL used for persisted settings and imported cards. */
fun normalizeRelayUrl(value: String): String {
    return uniffi.cruisemesh_core.normalizeRelayUrl(value)
}

/** Persists the optional family relay configuration used for QR sharing and fallback sync. */
object RelayConfigStore {

    fun load(context: Context): RelayConfig? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val relayUrl = normalizeRelayUrl(prefs.getString(PREF_RELAY_URL, null).orEmpty())
        val relayToken = prefs.getString(PREF_RELAY_TOKEN, null)?.trim().orEmpty()
        if (relayUrl.isEmpty() || relayToken.isEmpty()) return null
        return RelayConfig(relayUrl, relayToken)
    }

    /**
     * @param durable when the caller is about to exit the process (restore),
     *   write synchronously so the endpoint cannot be lost in flight.
     */
    fun save(context: Context, relayUrl: String, relayToken: String, durable: Boolean = false) {
        val normalizedUrl = normalizeRelayUrl(relayUrl)
        val normalizedToken = relayToken.trim()
        val cleared = normalizedUrl.isEmpty() || normalizedToken.isEmpty()
        val next = if (cleared) null else RelayConfig(normalizedUrl, normalizedToken)
        // T23: only a real change bumps the epoch. Settings screens re-save on
        // every keystroke, and a no-op save must not make contacts re-apply an
        // endpoint they already hold.
        if (next == load(context)) return

        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
        if (next == null) {
            prefs.remove(PREF_RELAY_URL).remove(PREF_RELAY_TOKEN)
        } else {
            prefs.putString(PREF_RELAY_URL, next.relayUrl)
                .putString(PREF_RELAY_TOKEN, next.relayToken)
        }
        prefs.putLong(PREF_RELAY_EPOCH, nextRelayEpoch(context)).persist(durable)
    }

    /**
     * T23: the current epoch of this device's own relay endpoint. `0` means it
     * has never changed since install, so there is nothing to announce.
     */
    fun relayEpoch(context: Context): Long =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getLong(PREF_RELAY_EPOCH, 0L)

    /** T23: the newest epoch already fanned out to contacts. */
    fun announcedRelayEpoch(context: Context): Long =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getLong(PREF_ANNOUNCED_RELAY_EPOCH, 0L)

    /** T23: records that [epoch] has been queued to every contact. */
    fun markRelayEpochAnnounced(context: Context, epoch: Long) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putLong(PREF_ANNOUNCED_RELAY_EPOCH, epoch)
            .apply()
    }

    /**
     * Wall clock, but never at or below the previous value: a backwards clock
     * (manual change, NTP correction) must not mint an epoch a contact would
     * ignore as stale, which would strand them on a dead endpoint forever.
     */
    private fun nextRelayEpoch(context: Context): Long =
        maxOf(System.currentTimeMillis(), relayEpoch(context) + 1)

    fun shareOnline(context: Context): Boolean =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(PREF_SHARE_ONLINE, true)

    /**
     * @param durable when the caller is about to exit the process (restore),
     *   write synchronously so the choice cannot be lost in flight.
     */
    fun setShareOnline(context: Context, enabled: Boolean, durable: Boolean = false) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(PREF_SHARE_ONLINE, enabled)
            .persist(durable)
    }

    /**
     * Records the Cruise Pass this device is actually using, once per launch.
     *
     * Without it a shared log cannot answer the first question anyone asks
     * about a relay problem -- is this phone even configured, and with which
     * pass? A log full of relay silence looks identical whether the pass is
     * missing, pointed at a dead host, or working perfectly with nothing to
     * carry.
     */
    fun logSummary(context: Context) {
        val config = load(context)
        if (config == null) {
            Log.i(RelayClient.TAG, "Relay not configured on this device (no Cruise Pass)")
            return
        }
        Log.i(
            RelayClient.TAG,
            "Relay configured: host=${hostOf(config.relayUrl)} " +
                "token=${tokenPrefix(config.relayToken)}… " +
                "epoch=${relayEpoch(context)} shareOnline=${shareOnline(context)}",
        )
    }

    private fun hostOf(url: String): String =
        runCatching { java.net.URL(url).host }.getOrNull() ?: "unparseable"

    /**
     * The first eight characters only. Enough to tell one family's pass from
     * another's, and from the shared tester pass, while staying useless to
     * anyone who reads the file: it is a bearer credential, so the full value
     * must never reach a share sheet.
     */
    internal fun tokenPrefix(token: String): String = token.take(8)
}
