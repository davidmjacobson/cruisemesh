package com.cruisemesh.app.relay

import org.junit.Assert.assertEquals
import org.junit.Test

class RelayPushBackoffTest {

    @Test
    fun `a fresh backoff starts at the initial delay`() {
        val backoff = RelayPushBackoff(initialBackoffMs = 2_000L)
        assertEquals(2_000L, backoff.nextDelayMs())
    }

    @Test
    fun `delay doubles on each consecutive failure up to the cap`() {
        val backoff = RelayPushBackoff(initialBackoffMs = 1_000L, maxBackoffMs = 5_000L)

        backoff.recordFailure()
        assertEquals(2_000L, backoff.nextDelayMs())

        backoff.recordFailure()
        assertEquals(4_000L, backoff.nextDelayMs())

        backoff.recordFailure() // would be 8_000, capped at 5_000
        assertEquals(5_000L, backoff.nextDelayMs())

        backoff.recordFailure() // stays capped
        assertEquals(5_000L, backoff.nextDelayMs())
    }

    @Test
    fun `recordSuccess resets the delay back to the initial value`() {
        val backoff = RelayPushBackoff(initialBackoffMs = 1_000L, maxBackoffMs = 60_000L)
        repeat(5) { backoff.recordFailure() }
        assertEquals(32_000L, backoff.nextDelayMs())

        backoff.recordSuccess()

        assertEquals(1_000L, backoff.nextDelayMs())
    }

    @Test
    fun `never overflows even after many consecutive failures`() {
        val backoff = RelayPushBackoff(initialBackoffMs = 1_000L, maxBackoffMs = 60_000L)
        repeat(1_000) { backoff.recordFailure() }
        assertEquals(60_000L, backoff.nextDelayMs())
    }

    @Test
    fun `empty relay hints (fresh onboarding, no contacts yet) back off to the cap like any other failure`() {
        // FA13 regression: RelayPushClient.connect() treats an empty hint
        // set (relayd would 400 a hint-less subscribe) as a failure and
        // calls recordFailure() before scheduling the retry -- otherwise a
        // freshly onboarded device with zero contacts/groups would retry at
        // the 2s floor forever instead of backing off like any other
        // dropped connection.
        val backoff = RelayPushBackoff(initialBackoffMs = 2_000L, maxBackoffMs = 60_000L)

        repeat(20) { backoff.recordFailure() }

        assertEquals(60_000L, backoff.nextDelayMs())
    }

    @Test
    fun `a never-failed backoff reports zero consecutive failures worth of delay growth`() {
        // Sanity check that a brand new instance and a recordSuccess()-reset
        // instance are indistinguishable -- both start the reconnect loop at
        // the floor, never at some remembered "give up" state.
        val fresh = RelayPushBackoff(initialBackoffMs = 3_000L, maxBackoffMs = 30_000L)
        val reset = RelayPushBackoff(initialBackoffMs = 3_000L, maxBackoffMs = 30_000L)
        repeat(3) { reset.recordFailure() }
        reset.recordSuccess()

        assertEquals(fresh.nextDelayMs(), reset.nextDelayMs())
    }
}
