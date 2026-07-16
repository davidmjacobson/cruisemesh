package com.cruisemesh.app.mesh

import android.content.Context
import com.cruisemesh.app.chat.UserIdHex
import java.util.Base64

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
        val signature = listOf(
            networkId,
            host,
            port.toString(),
            Base64.getUrlEncoder().withoutPadding().encodeToString(instanceToken),
        ).joinToString("|")
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val previous = prefs.getString(key, null)?.split('\n', limit = 2)
        val sameSignature = previous?.getOrNull(0) == signature
        val sentAt = previous?.getOrNull(1)?.toLongOrNull() ?: 0
        if (sameSignature && nowMs - sentAt < RESEND_INTERVAL_MS) return false
        prefs.edit().putString(key, "$signature\n$nowMs").apply()
        return true
    }

    private const val RESEND_INTERVAL_MS = 5 * 60 * 1_000L
}
