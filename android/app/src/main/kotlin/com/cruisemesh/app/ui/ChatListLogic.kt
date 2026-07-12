package com.cruisemesh.app.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_GROUP_INVITE
import com.cruisemesh.app.media.isVisibleChatKind
import uniffi.cruisemesh_core.StoredMessage
import java.util.Calendar
import java.text.SimpleDateFormat
import java.util.Locale

object ChatListLogic {

    fun displayNameOrId(name: String, displayId: String): String =
        if (name.isNotBlank() && name != "Unknown") name else displayId

    fun avatarHueAndInitials(userId: ByteArray, name: String, displayId: String): Pair<Color, String> {
        val hue = (userId.fold(0) { acc, byte -> acc + byte.toInt() } and 0xFF) / 255f
        val color = Color.hsv(hue * 360f, 0.5f, 0.7f)

        val initials = if (name.isNotBlank() && name != "Unknown") {
            name.take(2).uppercase()
        } else {
            // displayId is like CM-K7QX...
            val cleaned = displayId.removePrefix("CM-")
            cleaned.take(2).uppercase()
        }
        return color to initials
    }

    fun avatarTextColor(background: Color): Color =
        if (background.luminance() > 0.58f) Color.Black else Color.White

    fun avatarContentDescription(name: String, displayId: String): String =
        "Avatar for ${displayNameOrId(name, displayId)}"

    fun unreadBadgeText(count: Int): String =
        if (count > 99) "99+" else count.coerceAtLeast(0).toString()

    fun formatRelativeTime(timestampMs: Long, nowMs: Long = System.currentTimeMillis()): String {
        val now = Calendar.getInstance().apply { timeInMillis = nowMs }
        val then = Calendar.getInstance().apply { timeInMillis = timestampMs }
        val diffMs = nowMs - timestampMs
        
        val isSameDay = now.get(Calendar.YEAR) == then.get(Calendar.YEAR) && 
                        now.get(Calendar.DAY_OF_YEAR) == then.get(Calendar.DAY_OF_YEAR)
                        
        if (isSameDay) {
            return SimpleDateFormat("h:mm a", Locale.getDefault()).format(then.time)
        }
        
        // within 7 days
        if (diffMs in 0 until (7 * 24 * 60 * 60 * 1000L)) {
            return SimpleDateFormat("EEE", Locale.getDefault()).format(then.time)
        }
        
        return SimpleDateFormat("MMM d", Locale.getDefault()).format(then.time)
    }

    fun computeUnread(messages: List<StoredMessage>, ownUserId: ByteArray, readThrough: ULong): Int {
        return messages.count {
            isVisibleChatKind(it.kind) &&
                !it.senderUserId.contentEquals(ownUserId) &&
                it.lamport > readThrough
        }
    }

    /**
     * Group unread across multi-sender streams: for each other sender, count
     * messages above our local READ watermark for that sender (stored in
     * outgoing_receipts; group wire receipts are deferred).
     */
    fun computeGroupUnread(
        messages: List<StoredMessage>,
        ownUserId: ByteArray,
        readThroughForSender: (ByteArray) -> ULong,
    ): Int {
        return messages.count { msg ->
            isVisibleChatKind(msg.kind) &&
                !msg.senderUserId.contentEquals(ownUserId) &&
                msg.lamport > readThroughForSender(msg.senderUserId)
        }
    }

    /** Last message shown in the conversation list (hides friend-request noise). */
    fun lastVisibleMessage(messages: List<StoredMessage>): StoredMessage? =
        messages.filter { isVisibleChatKind(it.kind) }.maxByOrNull { it.timestamp }

    fun previewText(message: StoredMessage, groupName: String? = null): String {
        return when (message.kind) {
            KIND_ATTACHMENT_MANIFEST ->
                AttachmentPayload.previewLabel(AttachmentPayload.decode(message.payload))
            KIND_GROUP_INVITE ->
                if (groupName != null) "Group created: $groupName" else "Group invite"
            else -> String(message.payload, Charsets.UTF_8)
        }
    }
}
