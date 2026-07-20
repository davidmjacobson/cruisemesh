package com.cruisemesh.app.notify

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import com.cruisemesh.app.MainActivity
import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.coreContactDisplayName

private const val TAG = "MessageNotifier"

/**
 * Distinct from [com.cruisemesh.app.mesh.MeshService]'s IMPORTANCE_LOW
 * foreground-service channel (`"cruisemesh_mesh"`): messages should heads-up
 * and make noise; the always-on "relaying nearby" notification should not.
 */
private const val MESSAGE_CHANNEL_ID = "cruisemesh_messages"

/**
 * Posts the user-facing "new message" notification for an incoming chat
 * message.
 *
 * ### Caller contract (integration point, wired from the mesh layer)
 *
 * This deliberately does NOT consult [ChatVisibility]: suppression is the
 * caller's decision, made where the incoming message is stored (see
 * `MeshService.handleIncomingText`). The expected call site is
 *
 * ```
 * if (!ChatVisibility.isVisible(contact.userId)) {
 *     MessageNotifier.notifyIncomingMessage(context, contact, text)
 * }
 * ```
 *
 * Consequence of "notify on every newly stored, non-visible message": older
 * messages arriving in a burst via reconnect catch-up each notify too. Since
 * notifications collapse to one per chat (stable id, see below) that means
 * one notification showing the last message of the burst -- acceptable for
 * now; smarter batching/age cutoffs are a later refinement.
 */
object MessageNotifier {

    /**
     * Intent extra on [MainActivity] carrying the [UserIdHex]-encoded userId
     * (or group id) of the chat a notification tap should open.
     */
    const val EXTRA_CHAT_USER_ID_HEX = "com.cruisemesh.app.extra.CHAT_USER_ID_HEX"

    /**
     * When true, [EXTRA_CHAT_USER_ID_HEX] is a group id and the app should open
     * the group chat route rather than a 1:1 contact chat.
     */
    const val EXTRA_CHAT_IS_GROUP = "com.cruisemesh.app.extra.CHAT_IS_GROUP"
    const val ACTION_REPLY = "com.cruisemesh.app.action.REPLY"
    const val ACTION_MARK_READ = "com.cruisemesh.app.action.MARK_READ"
    const val REMOTE_INPUT_REPLY = "com.cruisemesh.app.input.REPLY"

    /**
     * Posts (or updates -- one notification per chat, keyed by a stable id
     * derived from [Contact.userId]) a notification showing [contact]'s name
     * and [text]. Tapping it opens [MainActivity] directly on that chat.
     *
     * Safe to call with POST_NOTIFICATIONS denied on Android 13+: logs at
     * INFO and returns without posting.
     */
    fun notifyIncomingMessage(context: Context, contact: Contact, text: String) {
        postChatNotification(
            context = context,
            chatId = contact.userId,
            title = coreContactDisplayName(contact),
            text = text,
            deepLinkHex = UserIdHex.encode(contact.userId),
            isGroup = false,
        )
    }

    /** Announces that an authenticated mutual friend request imported a new friend. */
    fun notifyFriendAdded(context: Context, contact: Contact) {
        postChatNotification(
            context = context,
            chatId = contact.userId,
            title = contact.name,
            text = "${contact.name} added you. Say hi.",
            deepLinkHex = UserIdHex.encode(contact.userId),
            isGroup = false,
        )
    }

    /**
     * Posts (or updates) a notification for an incoming group message. Tapping
     * opens the group chat via [EXTRA_CHAT_IS_GROUP] + [EXTRA_CHAT_USER_ID_HEX]
     * (the hex is the group id).
     */
    fun notifyIncomingGroupMessage(
        context: Context,
        group: Group,
        senderName: String,
        text: String,
    ) {
        val body = if (text.startsWith("Added you to ")) text else "$senderName: $text"
        postChatNotification(
            context = context,
            chatId = group.id,
            title = group.name,
            text = body,
            deepLinkHex = UserIdHex.encode(group.id),
            isGroup = true,
        )
    }

    /**
     * Dismisses the notification (if any) for [chatId]'s chat. Called when
     * that chat becomes visible so a notification posted while the app was
     * backgrounded clears the moment the user is looking at the chat.
     * [chatId] is the same key used to post it: the contact userId for a 1:1
     * chat, the group id for a group chat.
     */
    fun cancel(context: Context, chatId: ByteArray) {
        context.getSystemService(NotificationManager::class.java)
            ?.cancel(chatId.contentHashCode())
    }

    private fun postChatNotification(
        context: Context,
        chatId: ByteArray,
        title: String,
        text: String,
        deepLinkHex: String,
        isGroup: Boolean,
    ) {
        if (ChatMuteStore.isMuted(context, chatId)) return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            Log.i(TAG, "POST_NOTIFICATIONS not granted; skipping notification for incoming message")
            return
        }

        val manager = context.getSystemService(NotificationManager::class.java)
        ensureChannel(manager)

        // Stable per-chat id: the same chat always updates its one
        // notification instead of stacking, and it doubles as the
        // PendingIntent request code so intents for different chats never
        // collide (same request code + FLAG_UPDATE_CURRENT would otherwise
        // silently rewrite another chat's tap target).
        val notificationId = chatId.contentHashCode()

        val tapIntent = Intent(context, MainActivity::class.java).apply {
            putExtra(EXTRA_CHAT_USER_ID_HEX, deepLinkHex)
            putExtra(EXTRA_CHAT_IS_GROUP, isGroup)
            // Resume the existing task if the app is already running
            // (launchMode="singleTop" routes this through onNewIntent).
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val contentIntent = PendingIntent.getActivity(
            context,
            notificationId,
            tapIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        fun actionIntent(action: String, mutable: Boolean): PendingIntent {
            val intent = Intent(context, NotificationActionReceiver::class.java).apply {
                this.action = action
                putExtra(EXTRA_CHAT_USER_ID_HEX, deepLinkHex)
                putExtra(EXTRA_CHAT_IS_GROUP, isGroup)
            }
            return PendingIntent.getBroadcast(
                context,
                notificationId xor action.hashCode(),
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or if (mutable) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE,
            )
        }
        val sender = Person.Builder().setName(title).build()
        val replyInput = RemoteInput.Builder(REMOTE_INPUT_REPLY).setLabel("Reply").build()
        val replyAction = NotificationCompat.Action.Builder(
            android.R.drawable.ic_menu_send,
            "Reply",
            actionIntent(ACTION_REPLY, true),
        ).addRemoteInput(replyInput).build()
        val readAction = NotificationCompat.Action.Builder(
            android.R.drawable.ic_menu_view,
            "Mark as read",
            actionIntent(ACTION_MARK_READ, false),
        ).build()

        val notification = NotificationCompat.Builder(context, MESSAGE_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(
                NotificationCompat.MessagingStyle(Person.Builder().setName("You").build())
                    .setConversationTitle(if (isGroup) title else null)
                    .addMessage(text, System.currentTimeMillis(), sender),
            )
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setContentIntent(contentIntent)
            .setAutoCancel(true)
            .addAction(replyAction)
            .addAction(readAction)
            .setGroup("cruisemesh_conversations")
            .build()

        manager.notify(notificationId, notification)
    }

    /** Idempotent: `createNotificationChannel` is a no-op for an existing id. */
    private fun ensureChannel(manager: NotificationManager) {
        val channel = NotificationChannel(
            MESSAGE_CHANNEL_ID,
            "Messages",
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = "Incoming CruiseMesh messages"
        }
        manager.createNotificationChannel(channel)
    }
}
