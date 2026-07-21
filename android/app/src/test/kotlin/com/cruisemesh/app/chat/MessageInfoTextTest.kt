package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.StoredMessage

class MessageInfoTextTest {
    private fun message(lamport: ULong = 42uL) = StoredMessage(
        senderUserId = byteArrayOf(1),
        chatId = byteArrayOf(2),
        lamport = lamport,
        timestamp = 1_783_608_000_000L,
        kind = 1u.toUByte(),
        payload = "hello".toByteArray(),
    )

    private fun MessageInfoRow.asSentence() = this as MessageInfoRow.Sentence

    private fun labelValue(rows: List<MessageInfoRow>, label: String): MessageInfoRow.LabelValue? =
        rows.filterIsInstance<MessageInfoRow.LabelValue>().firstOrNull { it.label == label }

    @Test
    fun messageInfoDoesNotExposeLamportClock() {
        val rows = messageInfoRows(message(lamport = 42uL), isOwn = true, tick = TickStatus.READ)

        assertTrue(rows.any { it is MessageInfoRow.Sentence && it.text == "Sent by you" })
        assertTrue(labelValue(rows, "Status")?.value?.startsWith("Read") == true)
        assertFalse(rows.toString().contains("Lamport"))
        assertFalse(rows.toString().contains("42"))
    }

    @Test
    fun messageInfoShowsLocalArrivalRouteAndHops() {
        val arrival = MessageArrival(transport = 2u, hopsTaken = 2u, receivedAt = 1_783_608_000_500L)

        val rows = messageInfoRows(message(), isOwn = false, tick = null, arrival = arrival)

        val arrivalRow = rows.last().asSentence()
        assertTrue(arrivalRow.text.startsWith("Arrived via relay · ~2 hops ·"))
    }

    @Test
    fun messageInfoUsesSingularHopLabel() {
        val arrival = MessageArrival(transport = 1u, hopsTaken = 1u, receivedAt = 1_783_608_000_500L)

        val rows = messageInfoRows(message(), isOwn = false, tick = null, arrival = arrival)

        val arrivalRow = rows.last().asSentence()
        assertTrue(arrivalRow.text.startsWith("Arrived via another device over BLE · ~1 hop ·"))
    }

    /**
     * FA: the old renderer built one string and split each line on its first
     * ":" to fake "Label: value" styling. The arrival sentence's own
     * "h:mm a" time (e.g. "5:14 PM") always contains a colon, and with no
     * other colon earlier in the line, that split fired *inside the time* --
     * "...· 5" on one line, "14 PM" on the next. [messageInfoRows] never
     * infers structure from colons: the arrival line is always built as a
     * single [MessageInfoRow.Sentence], so its embedded colon can't be
     * mistaken for a label separator no matter what time it renders.
     */
    @Test
    fun arrivalRowStaysOneSentenceEvenThoughItsRenderedTimeContainsAColon() {
        // receivedAt formats via "h:mm a", which always contains a colon
        // between hour and minute -- this is not a specific-clock-value bug.
        val arrival = MessageArrival(transport = 1u, hopsTaken = 0u, receivedAt = 1_783_608_000_500L)

        val rows = messageInfoRows(message(), isOwn = false, tick = null, arrival = arrival)

        // Exactly one row carries the arrival info, and it's a Sentence, not
        // a (label, value) pair split out of the colon in its own time.
        val arrivalRows = rows.filter { it is MessageInfoRow.Sentence && it.text.startsWith("Arrived via") }
        assertEquals(1, arrivalRows.size)
        val arrivalText = arrivalRows.single().asSentence().text
        assertTrue("expected the rendered time to contain a colon", arrivalText.contains(":"))
        // Nothing downstream ever treats this row as a LabelValue.
        assertTrue(arrivalRows.single() is MessageInfoRow.Sentence)
    }

    @Test
    fun outgoingMessageInfoShowsTheDeliveryConfirmationRoute() {
        // T6: the route is resolved from the delivery receipt's watermark
        // (transport -> route) by the caller and passed in.
        val rows = messageInfoRows(
            message(),
            isOwn = true,
            tick = TickStatus.DELIVERED,
            deliveredViaRoute = transportRouteText(3),
        )

        assertTrue(labelValue(rows, "Status")?.value?.startsWith("Delivered") == true)
        assertTrue(rows.any { it is MessageInfoRow.Sentence && it.text == "Delivery confirmed via local Wi-Fi" })
        assertFalse(rows.any { it is MessageInfoRow.Sentence && it.text.startsWith("Arrived via") })
    }

    @Test
    fun outgoingMessageInfoOmitsConfirmationRouteWhenUnknown() {
        val rows = messageInfoRows(
            message(),
            isOwn = true,
            tick = TickStatus.DELIVERED,
            deliveredViaRoute = null,
        )

        assertTrue(labelValue(rows, "Status")?.value?.startsWith("Delivered") == true)
        assertNull(rows.filterIsInstance<MessageInfoRow.Sentence>().firstOrNull { it.text.startsWith("Delivery confirmed via") })
    }
}
