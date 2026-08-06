package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class FamilyRelayBackpressureTest {

    @Test
    fun `request pacer caps a phone at two requests per second`() {
        val pacer = FamilyRelayRequestPacer()

        assertEquals(0L, pacer.reserve(10_000L))
        assertEquals(500L, pacer.reserve(10_000L))
        assertEquals(750L, pacer.reserve(10_250L))
        assertEquals(0L, pacer.reserve(12_000L))
    }

    @Test
    fun `three family clients recover on staggered deadlines`() {
        val identityHashes = listOf(101, 202, 303)
        val clients = identityHashes.map { FamilyRelayBackoff() }

        val firstRetryDelays = clients.zip(identityHashes).map { (client, identityHash) ->
            client.onRateLimited(retryAfterMs = 1_000L, identityHash = identityHash)
        }

        assertEquals(3, firstRetryDelays.toSet().size)
        assertTrue(firstRetryDelays.all { it in 1_000L..2_000L })

        val secondRetryDelays = clients.zip(identityHashes).map { (client, identityHash) ->
            client.onRateLimited(retryAfterMs = 1_000L, identityHash = identityHash)
        }
        assertTrue(secondRetryDelays.all { it >= 2_000L })

        clients.forEach { it.onSuccessfulPass() }
        val recoveredRetryDelays = clients.zip(identityHashes).map { (client, identityHash) ->
            client.onRateLimited(retryAfterMs = 1_000L, identityHash = identityHash)
        }
        assertEquals(firstRetryDelays, recoveredRetryDelays)
    }

    @Test
    fun `server retry after remains the minimum quiet period`() {
        val delayMs = familyRelayBackoffDelayMs(
            retryAfterMs = 15_000L,
            consecutiveRateLimits = 1,
            identityHash = 42,
        )

        assertTrue(delayMs in 15_000L..16_000L)
    }

    @Test
    fun `exponential retry is capped before jitter`() {
        val delayMs = familyRelayBackoffDelayMs(
            retryAfterMs = 1_000L,
            consecutiveRateLimits = 100,
            identityHash = 0,
        )

        assertEquals(FAMILY_RELAY_BACKOFF_CAP_MS, delayMs)
    }
}
