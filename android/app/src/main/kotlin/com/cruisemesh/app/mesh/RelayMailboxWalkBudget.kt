package com.cruisemesh.app.mesh

/**
 * A relay mailbox may contain years of deliberately unacked proxy rows. A
 * restored legacy backup has no persisted fetch frontier, so its first pass
 * starts at zero; walking until the server returns an empty page can then run
 * thousands of sequential requests without yielding.
 *
 * Bound both request count and envelope work. The engine persists each safe
 * page's cursor before consulting this policy, then schedules a continuation
 * pass from that frontier.
 */
internal const val RELAY_MAILBOX_MAX_PAGES_PER_PASS = 4
internal const val RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS = 512
internal const val RELAY_MAILBOX_CONTINUATION_DELAY_MS = 1_000L

internal enum class RelayMailboxWalkAction {
    CONTINUE,
    YIELD_AND_SCHEDULE_CONTINUATION,
}

internal fun relayMailboxWalkAction(
    pagesFetched: Int,
    envelopesFetched: Int,
): RelayMailboxWalkAction = if (
    pagesFetched >= RELAY_MAILBOX_MAX_PAGES_PER_PASS ||
    envelopesFetched >= RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS
) {
    RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION
} else {
    RelayMailboxWalkAction.CONTINUE
}
