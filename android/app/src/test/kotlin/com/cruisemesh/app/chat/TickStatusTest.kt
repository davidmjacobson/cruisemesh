package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

class TickStatusTest {

    @Test
    fun `no receipts at all means sent`() {
        assertEquals(TickStatus.SENT, tickStatusFor(lamport = 1uL, deliveredThrough = 0uL, readThrough = 0uL))
    }

    @Test
    fun `a lamport at or below deliveredThrough is delivered`() {
        assertEquals(TickStatus.DELIVERED, tickStatusFor(lamport = 5uL, deliveredThrough = 5uL, readThrough = 0uL))
        assertEquals(TickStatus.DELIVERED, tickStatusFor(lamport = 3uL, deliveredThrough = 5uL, readThrough = 0uL))
    }

    @Test
    fun `a lamport above deliveredThrough is still just sent`() {
        assertEquals(TickStatus.SENT, tickStatusFor(lamport = 6uL, deliveredThrough = 5uL, readThrough = 0uL))
    }

    @Test
    fun `a lamport at or below readThrough is read`() {
        assertEquals(TickStatus.READ, tickStatusFor(lamport = 5uL, deliveredThrough = 5uL, readThrough = 5uL))
        assertEquals(TickStatus.READ, tickStatusFor(lamport = 2uL, deliveredThrough = 5uL, readThrough = 5uL))
    }

    @Test
    fun `read wins even if deliveredThrough lags behind readThrough`() {
        // A lost/delayed delivered receipt shouldn't hide a read receipt that
        // did arrive -- DESIGN.md §7.2 receipts are independent and best-effort.
        assertEquals(TickStatus.READ, tickStatusFor(lamport = 4uL, deliveredThrough = 1uL, readThrough = 4uL))
    }

    @Test
    fun `each watermark is an independent cumulative boundary`() {
        val deliveredThrough = 5uL
        val readThrough = 2uL
        assertEquals(TickStatus.READ, tickStatusFor(lamport = 1uL, deliveredThrough, readThrough))
        assertEquals(TickStatus.READ, tickStatusFor(lamport = 2uL, deliveredThrough, readThrough))
        assertEquals(TickStatus.DELIVERED, tickStatusFor(lamport = 3uL, deliveredThrough, readThrough))
        assertEquals(TickStatus.DELIVERED, tickStatusFor(lamport = 5uL, deliveredThrough, readThrough))
        assertEquals(TickStatus.SENT, tickStatusFor(lamport = 6uL, deliveredThrough, readThrough))
    }
}
