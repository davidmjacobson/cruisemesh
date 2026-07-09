package com.cruisemesh.app.ui

import androidx.compose.ui.graphics.Color
import uniffi.cruisemesh_core.StoredMessage
import java.util.Calendar
import java.text.SimpleDateFormat
import java.util.Locale

object ChatListLogic {

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

    fun formatRelativeTime(timestampMs: Long, nowMs: Long = System.currentTimeMillis()): String {
        val now = Calendar.getInstance().apply { timeInMillis = nowMs }
        val then = Calendar.getInstance().apply { timeInMillis = timestampMs }
        val diffMs = nowMs - timestampMs
        
        val isSameDay = now.get(Calendar.YEAR) == then.get(Calendar.YEAR) && 
                        now.get(Calendar.DAY_OF_YEAR) == then.get(Calendar.DAY_OF_YEAR)
                        
        if (isSameDay) {
            return SimpleDateFormat("h:mm a", Locale.US).format(then.time)
        }
        
        // within 7 days
        if (diffMs in 0 until (7 * 24 * 60 * 60 * 1000L)) {
            return SimpleDateFormat("EEE", Locale.US).format(then.time)
        }
        
        return SimpleDateFormat("MMM d", Locale.US).format(then.time)
    }

    fun computeUnread(messages: List<StoredMessage>, ownUserId: ByteArray, readThrough: ULong): Int {
        return messages.count { 
            !it.senderUserId.contentEquals(ownUserId) && it.lamport > readThrough 
        }
    }
}
