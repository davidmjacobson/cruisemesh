package com.cruisemesh.app.mesh

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GattWriteQueueTest {

    @Test
    fun `admitNext returns null for an address with nothing queued`() {
        val queue = GattWriteQueue()
        assertNull(queue.admitNext("AA:BB"))
    }

    @Test
    fun `the in-flight gate rejects a second concurrent admission for the same address`() {
        // This is the exact race BleCentral's unguarded check-then-act on
        // writeInFlight allowed: two threads both see "not in flight" and
        // both get a fragment to write, violating the one-in-flight-write-
        // per-connection GATT constraint. admitNext must let only one
        // through until completeWrite releases the slot.
        val queue = GattWriteQueue()
        queue.enqueue("AA:BB", listOf(byteArrayOf(1), byteArrayOf(2)))

        val first = queue.admitNext("AA:BB")
        assertArrayEquals(byteArrayOf(1), first)
        assertTrue(queue.isInFlight("AA:BB"))

        // A second admission attempt while the first is still in flight --
        // simulating a concurrent caller -- must be rejected even though a
        // fragment is waiting in the queue.
        val second = queue.admitNext("AA:BB")
        assertNull(second)
        assertEquals(1, queue.queuedCount("AA:BB"))

        queue.completeWrite("AA:BB")
        val third = queue.admitNext("AA:BB")
        assertArrayEquals(byteArrayOf(2), third)
    }

    @Test
    fun `queue drains in FIFO order across multiple enqueue calls`() {
        val queue = GattWriteQueue()
        queue.enqueue("AA:BB", listOf(byteArrayOf(1), byteArrayOf(2)))
        queue.enqueue("AA:BB", listOf(byteArrayOf(3)))

        val drained = mutableListOf<ByteArray>()
        repeat(3) {
            val fragment = queue.admitNext("AA:BB")
            assertTrue(fragment != null)
            drained += fragment!!
            queue.completeWrite("AA:BB")
        }

        assertEquals(3, drained.size)
        assertArrayEquals(byteArrayOf(1), drained[0])
        assertArrayEquals(byteArrayOf(2), drained[1])
        assertArrayEquals(byteArrayOf(3), drained[2])
        assertNull(queue.admitNext("AA:BB"))
    }

    @Test
    fun `clear drops both the queue and the in-flight reservation for one address`() {
        val queue = GattWriteQueue()
        queue.enqueue("AA:BB", listOf(byteArrayOf(1), byteArrayOf(2)))
        queue.admitNext("AA:BB") // reserve the slot, leaving one fragment queued
        assertTrue(queue.isInFlight("AA:BB"))
        assertEquals(1, queue.queuedCount("AA:BB"))

        queue.clear("AA:BB")

        assertFalse(queue.isInFlight("AA:BB"))
        assertEquals(0, queue.queuedCount("AA:BB"))
        assertNull(queue.admitNext("AA:BB"))
    }

    @Test
    fun `clear on one address never affects another address's queue`() {
        val queue = GattWriteQueue()
        queue.enqueue("AA:BB", listOf(byteArrayOf(1)))
        queue.enqueue("CC:DD", listOf(byteArrayOf(9)))

        queue.clear("AA:BB")

        assertNull(queue.admitNext("AA:BB"))
        assertArrayEquals(byteArrayOf(9), queue.admitNext("CC:DD"))
    }

    @Test
    fun `clearAll drops every address's queue and in-flight state -- e g on a full stop`() {
        val queue = GattWriteQueue()
        queue.enqueue("AA:BB", listOf(byteArrayOf(1)))
        queue.enqueue("CC:DD", listOf(byteArrayOf(2)))
        queue.admitNext("AA:BB")

        queue.clearAll()

        assertFalse(queue.isInFlight("AA:BB"))
        assertNull(queue.admitNext("AA:BB"))
        assertNull(queue.admitNext("CC:DD"))
    }
}
