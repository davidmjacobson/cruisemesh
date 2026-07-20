package com.cruisemesh.app.chat

import android.content.Context
import android.util.Base64

object DraftStore {
    private const val PREFS = "cruisemesh_chat_drafts"

    fun load(context: Context, chatId: ByteArray): String =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(key(chatId), "").orEmpty()

    fun save(context: Context, chatId: ByteArray, text: String) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val previous = prefs.getString(key(chatId), "").orEmpty()
        val edit = prefs.edit()
        if (text.trim('\n', '\r').isEmpty()) edit.remove(key(chatId)) else edit.putString(key(chatId), text)
        edit.apply()
        // XP1: don't fire chat-changed on every keystroke -- the chat list re-reads
        // the live draft text on its own reload passes, so only the presence
        // transition (a draft appearing/disappearing) needs to be announced.
        if (DraftChangeSignal.shouldNotify(previous, text)) {
            ChatEvents.notifyChatChanged(chatId)
        }
    }

    private fun key(chatId: ByteArray): String =
        Base64.encodeToString(chatId, Base64.NO_WRAP or Base64.URL_SAFE)
}
