package com.cruisemesh.app.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
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
}
