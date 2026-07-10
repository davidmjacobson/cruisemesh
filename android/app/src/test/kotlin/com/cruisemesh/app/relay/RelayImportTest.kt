package com.cruisemesh.app.relay

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RelayImportTest {
    @Test
    fun `card with relay is adopted as fallback when we have none`() {
        val decision = RelayImport.decide(
            cardHasRelay = true,
            existingHasRelay = false,
            hasOwnFallback = false,
        )
        assertTrue(decision.adoptAsFallback)
        assertEquals(ContactRelaySource.CARD, decision.contactSource)
    }

    @Test
    fun `card with relay does not overwrite an existing fallback`() {
        val decision = RelayImport.decide(
            cardHasRelay = true,
            existingHasRelay = true,
            hasOwnFallback = true,
        )
        assertFalse(decision.adoptAsFallback)
        assertEquals(ContactRelaySource.CARD, decision.contactSource)
    }

    @Test
    fun `blank card keeps an existing contact relay instead of wiping it`() {
        val decision = RelayImport.decide(
            cardHasRelay = false,
            existingHasRelay = true,
            hasOwnFallback = false,
        )
        assertFalse(decision.adoptAsFallback)
        assertEquals(ContactRelaySource.EXISTING, decision.contactSource)
    }

    @Test
    fun `blank card with no existing relay just takes the card as-is`() {
        val decision = RelayImport.decide(
            cardHasRelay = false,
            existingHasRelay = false,
            hasOwnFallback = false,
        )
        assertFalse(decision.adoptAsFallback)
        assertEquals(ContactRelaySource.CARD, decision.contactSource)
    }

    @Test
    fun `blank card never adopts a fallback even without one`() {
        val decision = RelayImport.decide(
            cardHasRelay = false,
            existingHasRelay = false,
            hasOwnFallback = false,
        )
        assertFalse(decision.adoptAsFallback)
    }
}
