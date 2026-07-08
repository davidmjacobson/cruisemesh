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
import androidx.core.content.ContextCompat
import com.cruisemesh.app.MainActivity
import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.Contact

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
     * of the chat a notification tap should open.
     */
    const val EXTRA_CHAT_USER_ID_HEX = "com.cruisemesh.app.extra.CHAT_USER_ID_HEX"

    /**
     * Posts (or updates -- one notification per chat, keyed by a stable id
     * derived from [Contact.userId]) a notification showing [contact]'s name
     * and [text]. Tapping it opens [MainActivity] directly on that chat.
     *
     * Safe to call with POST_NOTIFICATIONS denied on Android 13+: logs at
     * INFO and returns without posting.
     */
    fun notifyIncomingMessage(context: Context, contact: Contact, text: String) {
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
        val notificationId = contact.userId.contentHashCode()

        val tapIntent = Intent(context, MainActivity::class.java).apply {
            putExtra(EXTRA_CHAT_USER_ID_HEX, UserIdHex.encode(contact.userId))
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

        val notification = NotificationCompat.Builder(context, MESSAGE_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle(contact.name)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setContentIntent(contentIntent)
            .setAutoCancel(true)
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
