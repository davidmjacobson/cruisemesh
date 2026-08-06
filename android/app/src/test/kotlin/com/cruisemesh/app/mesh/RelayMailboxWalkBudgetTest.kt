package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class RelayMailboxWalkBudgetTest {
    @Test
    fun `large mailbox yields after bounded pages and requests continuation`() {
        var pages = 0
        var envelopes = 0
        var continuationScheduled = false

        // Simulate a server that clamps each response to 16 rows. The old
        // until-empty loop could issue thousands of requests; this pass stops
        // after four even though it is far below the envelope ceiling.
        while (!continuationScheduled) {
            pages += 1
            envelopes += 16
            continuationScheduled = relayMailboxWalkAction(pages, envelopes) ==
                RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION
        }

        assertEquals(RELAY_MAILBOX_MAX_PAGES_PER_PASS, pages)
        assertEquals(64, envelopes)
    }

    @Test
    fun `envelope ceiling yields before page ceiling`() {
        assertEquals(
            RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION,
            relayMailboxWalkAction(pagesFetched = 2, envelopesFetched = 512),
        )
    }

    @Test
    fun `small mailbox continues toward its empty terminal page`() {
        assertEquals(
            RelayMailboxWalkAction.CONTINUE,
            relayMailboxWalkAction(pagesFetched = 1, envelopesFetched = 12),
        )
    }
}
