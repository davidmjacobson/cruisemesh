package com.cruisemesh.app.ui

import android.content.Context
import android.text.format.DateFormat
import java.util.Calendar
import java.util.TimeZone

private const val DEFAULT_GROUP_WINDOW_MS = 5 * 60 * 1000L

data class ConversationMessageMeta(
    val senderKey: String,
    val timestampMs: Long,
)

data class BubbleGrouping(
    val joinsPrevious: Boolean,
    val joinsNext: Boolean,
) {
    val showTimestamp: Boolean
        get() = !joinsNext
}

fun bubbleGroupingFor(
    messages: List<ConversationMessageMeta>,
    index: Int,
    groupingWindowMs: Long = DEFAULT_GROUP_WINDOW_MS,
    timeZone: TimeZone = TimeZone.getDefault(),
): BubbleGrouping {
    val current = messages[index]
    val previous = messages.getOrNull(index - 1)
    val next = messages.getOrNull(index + 1)
    return BubbleGrouping(
        joinsPrevious = previous != null && shouldGroup(previous, current, groupingWindowMs, timeZone),
        joinsNext = next != null && shouldGroup(current, next, groupingWindowMs, timeZone),
    )
}

fun formatConversationTimestamp(
    context: Context,
    timestampMs: Long,
    timeZone: TimeZone = TimeZone.getDefault(),
): String {
    val formatter = DateFormat.getTimeFormat(context)
    formatter.timeZone = timeZone
    return formatter.format(timestampMs)
}

private fun shouldGroup(
    first: ConversationMessageMeta,
    second: ConversationMessageMeta,
    groupingWindowMs: Long,
    timeZone: TimeZone,
): Boolean {
    if (first.senderKey != second.senderKey) {
        return false
    }
    val diffMs = second.timestampMs - first.timestampMs
    return diffMs in 0..groupingWindowMs && isSameDay(first.timestampMs, second.timestampMs, timeZone)
}

private fun isSameDay(firstMs: Long, secondMs: Long, timeZone: TimeZone): Boolean {
    val first = Calendar.getInstance(timeZone).apply { timeInMillis = firstMs }
    val second = Calendar.getInstance(timeZone).apply { timeInMillis = secondMs }
    return first.get(Calendar.YEAR) == second.get(Calendar.YEAR) &&
        first.get(Calendar.DAY_OF_YEAR) == second.get(Calendar.DAY_OF_YEAR)
}
