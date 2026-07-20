package com.cruisemesh.app.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.StoredMessage

class MessageInfoTextTest {
    @Test
    fun messageInfoDoesNotExposeLamportClock() {
        val message = StoredMessage(
            senderUserId = byteArrayOf(1),
            chatId = byteArrayOf(2),
            lamport = 42uL,
            timestamp = 1_783_608_000_000L,
            kind = 1u.toUByte(),
            payload = "hello".toByteArray(),
        )

        val info = messageInfoText(message, isOwn = true, tick = TickStatus.READ)

        assertTrue(info.contains("Sent by you"))
        assertTrue(info.contains("Status: Read"))
        assertFalse(info.contains("Lamport"))
        assertFalse(info.contains("42"))
    }

    @Test
    fun messageInfoShowsLocalArrivalRouteAndHops() {
        val message = StoredMessage(
            senderUserId = byteArrayOf(1),
            chatId = byteArrayOf(2),
            lamport = 3uL,
            timestamp = 1_783_608_000_000L,
            kind = 1u.toUByte(),
            payload = "hello".toByteArray(),
        )
        val arrival = MessageArrival(
            transport = 2u,
            hopsTaken = 2u,
            receivedAt = 1_783_608_000_500L,
        )

        val info = messageInfoText(message, isOwn = false, tick = null, arrival = arrival)

        assertTrue(info.contains("Arrived via relay · ~2 hops ·"))
    }

    @Test
    fun messageInfoUsesSingularHopLabel() {
        val message = StoredMessage(
            senderUserId = byteArrayOf(1),
            chatId = byteArrayOf(2),
            lamport = 3uL,
            timestamp = 1_783_608_000_000L,
            kind = 1u.toUByte(),
            payload = "hello".toByteArray(),
        )
        val arrival = MessageArrival(
            transport = 1u,
            hopsTaken = 1u,
            receivedAt = 1_783_608_000_500L,
        )

        val info = messageInfoText(message, isOwn = false, tick = null, arrival = arrival)

        assertTrue(info.contains("Arrived via another device over BLE · ~1 hop ·"))
    }

    @Test
    fun outgoingMessageInfoShowsTheDeliveryConfirmationRoute() {
        val message = StoredMessage(
            senderUserId = byteArrayOf(1),
            chatId = byteArrayOf(2),
            lamport = 3uL,
            timestamp = 1_783_608_000_000L,
            kind = 1u.toUByte(),
            payload = "hello".toByteArray(),
        )

        // T6: the route is resolved from the delivery receipt's watermark
        // (transport -> route) by the caller and passed in.
        val info = messageInfoText(
            message,
            isOwn = true,
            tick = TickStatus.DELIVERED,
            deliveredViaRoute = transportRouteText(3),
        )

        assertTrue(info.contains("Status: Delivered"))
        assertTrue(info.contains("Delivery confirmed via local Wi-Fi"))
        assertFalse(info.contains("Arrived via"))
    }

    @Test
    fun outgoingMessageInfoOmitsConfirmationRouteWhenUnknown() {
        val message = StoredMessage(
            senderUserId = byteArrayOf(1),
            chatId = byteArrayOf(2),
            lamport = 3uL,
            timestamp = 1_783_608_000_000L,
            kind = 1u.toUByte(),
            payload = "hello".toByteArray(),
        )

        val info = messageInfoText(
            message,
            isOwn = true,
            tick = TickStatus.DELIVERED,
            deliveredViaRoute = null,
        )

        assertTrue(info.contains("Status: Delivered"))
        assertFalse(info.contains("Delivery confirmed via"))
    }
}
