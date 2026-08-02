package com.cruisemesh.app.mesh

import android.content.Context
import com.cruisemesh.app.chat.UserIdHex

internal object LanCapabilityStore {
    private const val PREFS = "cruisemesh_lan_capabilities"
    private const val LAST_SEEN_PREFIX = "seen:"

    fun markSupported(
        context: Context,
        userId: ByteArray,
        nowMs: Long = System.currentTimeMillis(),
    ) {
        val key = UserIdHex.encode(userId)
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(key, true)
            .putLong(LAST_SEEN_PREFIX + key, nowMs)
            .apply()
    }

    fun isSupported(context: Context, userId: ByteArray): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getBoolean(UserIdHex.encode(userId), false)

    /**
     * When this contact last demonstrated LAN support, or null if it never
     * has -- including contacts marked supported by a build that predates
     * this timestamp, which stop motivating automatic sweeps until the next
     * link or endpoint hint records a fresh one. See
     * [lanCapabilityMotivatesScan].
     */
    fun lastSupportedAtMs(context: Context, userId: ByteArray): Long? =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getLong(LAST_SEEN_PREFIX + UserIdHex.encode(userId), 0L)
            .takeIf { it > 0L }

    @Synchronized
    fun shouldSendEndpoint(
        context: Context,
        userId: ByteArray,
        networkId: String,
        host: String,
        port: Int,
        instanceToken: ByteArray,
        nowMs: Long = System.currentTimeMillis(),
    ): Boolean {
        val key = "sent:${UserIdHex.encode(userId)}"
        val signature = lanEndpointSignature(networkId, host, port, instanceToken)
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (!shouldClaimLanEndpointSend(prefs.getString(key, null), signature, nowMs)) return false
        prefs.edit().putString(key, lanEndpointSendRecord(signature, nowMs)).apply()
        return true
    }
}
