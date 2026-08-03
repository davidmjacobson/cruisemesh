package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test

class RelayRerunPolicyTest {

    @Test
    fun `pending nudge with no backoff runs another pass`() {
        assertEquals(
            RelayRerunAction.RUN_AGAIN,
            relayRerunAction(pendingRequested = true, canSync = true, backoffRemainingMs = 0),
        )
        assertEquals(
            RelayRerunAction.RUN_AGAIN,
            relayRerunAction(pendingRequested = true, canSync = true, backoffRemainingMs = -5_000),
        )
    }

    @Test
    fun `pending nudge inside the advertised window coalesces into the retry timer`() {
        // The storm case: the pass that just finished recorded a 429 window,
        // and a nudge arrived while it was in flight. Re-running immediately
        // is exactly what melted the family rate budget.
        assertEquals(
            RelayRerunAction.SCHEDULE_RATE_LIMIT_RETRY,
            relayRerunAction(pendingRequested = true, canSync = true, backoffRemainingMs = 1),
        )
    }

    @Test
    fun `no pending nudge releases the thread regardless of backoff`() {
        assertEquals(
            RelayRerunAction.STOP,
            relayRerunAction(pendingRequested = false, canSync = true, backoffRemainingMs = 0),
        )
        assertEquals(
            RelayRerunAction.STOP,
            relayRerunAction(pendingRequested = false, canSync = true, backoffRemainingMs = 9_999),
        )
    }

    @Test
    fun `a pending nudge is dropped when syncing is impossible`() {
        assertEquals(
            RelayRerunAction.STOP,
            relayRerunAction(pendingRequested = true, canSync = false, backoffRemainingMs = 0),
        )
    }
}
