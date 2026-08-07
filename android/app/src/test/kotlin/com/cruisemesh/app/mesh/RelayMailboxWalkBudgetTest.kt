package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import uniffi.cruisemesh_core.RelayMailboxWalkAction
import uniffi.cruisemesh_core.relayMailboxContinuationDelayMs
import uniffi.cruisemesh_core.relayMailboxMaxEnvelopesPerPass
import uniffi.cruisemesh_core.relayMailboxMaxPagesPerPass
import uniffi.cruisemesh_core.relayMailboxWalkAction
import org.junit.Test

/**
 * The walk budget itself now lives in the Rust core (`core/src/relay_cursor.rs`),
 * so iOS answers these questions with the same numbers. What is left to pin
 * here is that the Android shell reaches the core policy rather than a local
 * copy of it -- the local copy is what let the two shells disagree, iOS having
 * had no budget at all.
 */
class RelayMailboxWalkBudgetTest {
    @Test
    fun `large mailbox yields after bounded pages and requests continuation`() {
        var pages = 0u
        var envelopes = 0u
        var continuationScheduled = false

        // Simulate a server that clamps each response to 16 rows. The old
        // until-empty loop could issue thousands of requests; this pass stops
        // after four even though it is far below the envelope ceiling.
        while (!continuationScheduled) {
            pages += 1u
            envelopes += 16u
            continuationScheduled = relayMailboxWalkAction(pages, envelopes) ==
                RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION
        }

        assertEquals(relayMailboxMaxPagesPerPass(), pages)
        assertEquals(64u, envelopes)
    }

    @Test
    fun `envelope ceiling yields before page ceiling`() {
        assertEquals(
            RelayMailboxWalkAction.YIELD_AND_SCHEDULE_CONTINUATION,
            relayMailboxWalkAction(
                pagesFetched = 2u,
                envelopesFetched = relayMailboxMaxEnvelopesPerPass(),
            ),
        )
    }

    @Test
    fun `small mailbox continues toward its empty terminal page`() {
        assertEquals(
            RelayMailboxWalkAction.CONTINUE_WALK,
            relayMailboxWalkAction(pagesFetched = 1u, envelopesFetched = 12u),
        )
    }

    @Test
    fun `both shells read the same budget from the core`() {
        assertEquals(4u, relayMailboxMaxPagesPerPass())
        assertEquals(512u, relayMailboxMaxEnvelopesPerPass())
        assertEquals(1_000L, relayMailboxContinuationDelayMs())
    }
}
