package com.cruisemesh.app.notify

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.GroupSender
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.IdentityStore

class NotificationActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val hex = intent.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX) ?: return
        val chatId = runCatching { UserIdHex.decode(hex) }.getOrNull() ?: return
        val isGroup = intent.getBooleanExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP, false)
        val identity = IdentityStore.load(context) ?: return
        val store = AppStore.get(context)

        when (intent.action) {
            MessageNotifier.ACTION_REPLY -> {
                val text = RemoteInput.getResultsFromIntent(intent)
                    ?.getCharSequence(MessageNotifier.REMOTE_INPUT_REPLY)?.toString()?.trim().orEmpty()
                if (text.isEmpty()) return
                if (isGroup) {
                    store.getGroup(chatId)?.let { GroupSender(store, identity).sendText(it, text) }
                } else {
                    store.getContact(chatId)?.let { RealMeshSender(store, identity).sendText(it, text) }
                }
            }
            MessageNotifier.ACTION_MARK_READ -> {
                val senderIds = if (isGroup) {
                    store.getGroup(chatId)?.memberUserIds.orEmpty().filterNot { it.contentEquals(identity.userId) }
                } else listOf(chatId)
                for (senderId in senderIds) {
                    val through = store.highestLamport(chatId, senderId)
                    if (through > 0uL) store.recordOutgoingReceipt(chatId, senderId, 1u, through)
                }
                ChatEvents.notifyChatChanged(chatId)
            }
        }
        MessageNotifier.cancel(context, chatId)
    }
}
