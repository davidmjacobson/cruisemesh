package com.cruisemesh.app.notify

import android.content.Context
import android.util.Base64

object ChatMuteStore {
    private const val PREFS = "cruisemesh_chat_mutes"

    fun isMuted(context: Context, chatId: ByteArray): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(key(chatId), false)

    fun setMuted(context: Context, chatId: ByteArray, muted: Boolean) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putBoolean(key(chatId), muted).apply()
    }

    private fun key(chatId: ByteArray): String = Base64.encodeToString(chatId, Base64.NO_WRAP or Base64.URL_SAFE)
}
