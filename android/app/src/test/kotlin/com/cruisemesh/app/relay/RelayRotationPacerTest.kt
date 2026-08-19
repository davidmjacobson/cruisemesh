package com.cruisemesh.app.relay

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** How often §10 step 2's rotate call may be made. Plain class, plain test. */
class RelayRotationPacerTest {

    private val now = 1_755_000_000_000L

    @Test
    fun `a fresh pacer allows the first attempt at once`() {
        val pacer = RelayRotationPacer()
        assertTrue(pacer.mayAttempt(now))
        assertEquals(0, pacer.consecutiveFailures)
    }

    @Test
    fun `a failure holds the next attempt off for exactly what it was given`() {
        val pacer = RelayRotationPacer()
        pacer.onFailure(now, 900_000L)

        assertFalse(pacer.mayAttempt(now))
        assertFalse(pacer.mayAttempt(now + 899_999L))
        assertTrue(pacer.mayAttempt(now + 900_000L))
        assertEquals(1, pacer.consecutiveFailures)
    }

    /**
     * A second failure inside an open window must not become permission to ask
     * sooner. Same rule as the relay pass's own quiet window, and for the same
     * reason: the shortest wait must never win.
     */
    @Test
    fun `a later shorter wait cannot pull an open window in`() {
        val pacer = RelayRotationPacer()
        pacer.onFailure(now, 3_600_000L)
        pacer.onFailure(now + 1_000L, 30_000L)

        assertFalse(pacer.mayAttempt(now + 60_000L))
        assertEquals(now + 3_600_000L, pacer.nextAttemptAtMs)
        assertEquals(2, pacer.consecutiveFailures)
    }

    @Test
    fun `settling clears the ladder so the next ceremony starts from the bottom`() {
        val pacer = RelayRotationPacer()
        pacer.onFailure(now, 3_600_000L)
        pacer.onSettled()

        assertTrue(pacer.mayAttempt(now))
        assertEquals(0, pacer.consecutiveFailures)
    }

    @Test
    fun `a negative delay is not a way to ask sooner`() {
        val pacer = RelayRotationPacer()
        pacer.onFailure(now, -5_000L)
        assertTrue(pacer.mayAttempt(now))
    }
}
