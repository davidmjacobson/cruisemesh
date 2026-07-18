package com.cruisemesh.app.mesh

import android.content.Context
import com.cruisemesh.app.chat.UserIdHex

internal object LanCapabilityStore {
    private const val PREFS = "cruisemesh_lan_capabilities"

    fun markSupported(context: Context, userId: ByteArray) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(UserIdHex.encode(userId), true)
            .apply()
    }

    fun isSupported(context: Context, userId: ByteArray): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getBoolean(UserIdHex.encode(userId), false)

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
