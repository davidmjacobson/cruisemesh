package com.cruisemesh.app.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatSummaryRefreshPolicyTest {
    @Test
    fun firstEventAlwaysReschedules() {
        assertTrue(
            ChatSummaryRefreshPolicy.shouldRescheduleDebounce(
                lastEventAtMs = 0L,
                eventAtMs = 1_000L,
                nowMs = 1_000L,
            ),
        )
    }

    @Test
    fun fireRequiresQuietWindowAfterLastEvent() {
        val last = 1_000L
        val debounce = ChatSummaryRefreshPolicy.DEBOUNCE_MS
        assertFalse(
            ChatSummaryRefreshPolicy.shouldFireRefresh(
                lastEventAtMs = last,
                scheduledFireAtMs = last + debounce,
                nowMs = last + debounce - 1,
            ),
        )
        assertTrue(
            ChatSummaryRefreshPolicy.shouldFireRefresh(
                lastEventAtMs = last,
                scheduledFireAtMs = last + debounce,
                nowMs = last + debounce,
            ),
        )
    }

    @Test
    fun stormOfEventsRequiresQuietBeforeFire() {
        // Simulate 10 events 10ms apart; fire only after quiet DEBOUNCE_MS.
        var last = 0L
        val start = 5_000L
        for (i in 0 until 10) {
            val t = start + i * 10L
            assertTrue(ChatSummaryRefreshPolicy.shouldRescheduleDebounce(last, t, t))
            last = t
        }
        assertFalse(
            ChatSummaryRefreshPolicy.shouldFireRefresh(
                lastEventAtMs = last,
                scheduledFireAtMs = last + ChatSummaryRefreshPolicy.DEBOUNCE_MS,
                nowMs = last + 50L,
            ),
        )
        assertTrue(
            ChatSummaryRefreshPolicy.shouldFireRefresh(
                lastEventAtMs = last,
                scheduledFireAtMs = last + ChatSummaryRefreshPolicy.DEBOUNCE_MS,
                nowMs = last + ChatSummaryRefreshPolicy.DEBOUNCE_MS,
            ),
        )
    }
}
