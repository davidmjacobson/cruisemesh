package com.cruisemesh.app.chat

/**
 * Pure policy for home chat-list refresh under mesh storms (G1).
 *
 * Field ANRs showed undebounced [ChatEvents] firing one full main-thread
 * `reloadSummaries` per accepted envelope. Consumers must conflate bursts so
 * a storm collapses to a bounded number of off-main refreshes.
 */
object ChatSummaryRefreshPolicy {
    /** Quiet window after the last change event before a refresh runs. */
    const val DEBOUNCE_MS: Long = 250L

    /**
     * Whether a change at [eventAtMs] should schedule a new debounce timer
     * given [lastEventAtMs] (0 if none) and [nowMs]. Always true for the first
     * event; subsequent events always reschedule (trailing debounce).
     */
    fun shouldRescheduleDebounce(lastEventAtMs: Long, eventAtMs: Long, nowMs: Long): Boolean {
        // Trailing debounce: every event reschedules. The timer fires only after
        // DEBOUNCE_MS of quiet. Kept pure so JVM tests pin the contract.
        return eventAtMs <= nowMs && (lastEventAtMs == 0L || eventAtMs >= lastEventAtMs)
    }

    /**
     * Whether a scheduled fire at [scheduledFireAtMs] is still valid given the
     * latest event time [lastEventAtMs] and [nowMs]. Invalid if a newer event
     * arrived after this fire was scheduled (caller should reschedule).
     */
    fun shouldFireRefresh(lastEventAtMs: Long, scheduledFireAtMs: Long, nowMs: Long): Boolean {
        if (nowMs < scheduledFireAtMs) return false
        // Fire only if the quiet window after lastEvent has elapsed.
        return nowMs >= lastEventAtMs + DEBOUNCE_MS
    }
}
