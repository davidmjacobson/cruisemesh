package com.cruisemesh.app.friending

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PendingEventQueueTest {
    @Test
    fun eventRemainsPendingUntilConsumed() {
        val queue = PendingEventQueue<String>()

        queue.enqueue("friend added")

        val pending = queue.events.value.single()
        assertEquals("friend added", pending.value)

        queue.consume(pending.id)
        assertTrue(queue.events.value.isEmpty())
    }

    @Test
    fun queueKeepsTheNewestEventsWithinItsCapacity() {
        val queue = PendingEventQueue<String>(capacity = 2)

        queue.enqueue("first")
        queue.enqueue("second")
        queue.enqueue("third")

        assertEquals(listOf("second", "third"), queue.events.value.map { it.value })
    }
}
