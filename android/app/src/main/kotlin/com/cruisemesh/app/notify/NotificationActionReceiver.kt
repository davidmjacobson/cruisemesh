package com.cruisemesh.app.notify

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.RemoteInput
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.GroupSender
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.IdentityStore
import java.util.concurrent.Executors

/**
 * Handles notification actions (`ACTION_REPLY` / `ACTION_MARK_READ`) posted
 * by [MessageNotifier].
 *
 * [RealMeshSender.sendText]/[GroupSender.sendText] seal the reply and write
 * it to SQLite, then re-encode and re-send every still-unacked outbound
 * envelope for the chat — including attachment blobs (see
 * `MeshSender.kt`'s `sendUnackedEnvelopes`). `ACTION_MARK_READ` writes one
 * outgoing-receipt row per group member. None of that belongs on the main
 * thread inside a `BroadcastReceiver`'s ~10s ANR budget, so [onReceive]
 * only does the cheap intent parsing itself and defers everything else to
 * [executor] via [goAsync], finishing the pending result whether the
 * background work succeeds or throws.
 */
class NotificationActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val hex = intent.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX) ?: return
        val chatId = runCatching { UserIdHex.decode(hex) }.getOrNull() ?: return
        val isGroup = intent.getBooleanExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP, false)
        val action = intent.action
        val appContext = context.applicationContext

        val pendingResult = goAsync()
        executor.execute {
            try {
                handleAction(appContext, action, chatId, isGroup, intent)
            } catch (t: Throwable) {
                Log.e(TAG, "Failed to handle notification action $action", t)
            } finally {
                pendingResult.finish()
            }
        }
    }

    /** Runs on [executor]. Mirrors the previous synchronous `onReceive` body exactly. */
    private fun handleAction(
        context: Context,
        action: String?,
        chatId: ByteArray,
        isGroup: Boolean,
        intent: Intent,
    ) {
        val identity = IdentityStore.load(context) ?: return
        val store = AppStore.get(context)

        when (action) {
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

    companion object {
        private const val TAG = "NotifActionReceiver"

        // Broadcast-triggered work (crypto sealing, SQLite writes, unacked-envelope
        // re-send) is sequential per chat by nature (store transactions), so a single
        // background thread is enough and keeps ordering between rapid-fire actions
        // (e.g. reply then mark-read on the same notification) deterministic.
        private val executor = Executors.newSingleThreadExecutor()
    }
}
