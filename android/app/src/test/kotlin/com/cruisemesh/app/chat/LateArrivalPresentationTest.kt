package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreMessageReceivedAt
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.coreLegacyDeviceId

private val OWN = byteArrayOf(9, 9)
private val THEM = byteArrayOf(1, 1)

private const val MINUTE = 60_000L
private const val HOUR = 60 * MINUTE

private fun message(sender: ByteArray, lamport: ULong, sentAt: Long): StoredMessage =
    StoredMessage(
        chatId = byteArrayOf(7),
        senderUserId = sender,
        lamport = lamport,
        timestamp = sentAt,
        kind = 1u,
        payload = ByteArray(0),
        senderDeviceId = coreLegacyDeviceId(),
    )

private fun arrival(sender: ByteArray, lamport: ULong, receivedAt: Long): CoreMessageReceivedAt =
    CoreMessageReceivedAt(senderUserId = sender, lamport = lamport, receivedAtMs = receivedAt)

class LateArrivalPresentationTest {
    @Test
    fun `a message carried above our reply reports when it arrived`() {
        val messages = listOf(
            message(OWN, 1u, 0),
            message(THEM, 1u, MINUTE),
            message(OWN, 2u, 30 * MINUTE),
        )
        val received = listOf(arrival(THEM, 1u, 3 * HOUR))

        val flagged = lateArrivalTimesByKey(messages, received, OWN)

        assertEquals(
            mapOf(messageStableKey(messages[1]) to 3 * HOUR),
            flagged,
        )
    }

    @Test
    fun `a thread that arrived in order says nothing`() {
        val messages = listOf(
            message(THEM, 1u, 0),
            message(THEM, 2u, HOUR),
        )
        val received = listOf(
            arrival(THEM, 1u, 3 * HOUR),
            arrival(THEM, 2u, 4 * HOUR),
        )

        assertTrue(lateArrivalTimesByKey(messages, received, OWN).isEmpty())
    }

    @Test
    fun `a message with no recorded arrival is never reported`() {
        // Legacy rows predate arrival diagnostics: no entry, so nothing to say.
        val messages = listOf(
            message(THEM, 1u, 0),
            message(OWN, 1u, 30 * MINUTE),
        )

        assertTrue(lateArrivalTimesByKey(messages, emptyList(), OWN).isEmpty())
    }

    @Test
    fun `arrival rows are matched by sender as well as lamport`() {
        // Two senders both at lamport 1: matching on lamport alone would
        // hand one sender's arrival time to the other's message.
        val messages = listOf(
            message(THEM, 1u, 0),
            message(OWN, 1u, 30 * MINUTE),
        )
        val received = listOf(
            arrival(THEM, 1u, 3 * HOUR),
            arrival(byteArrayOf(2, 2), 1u, 9 * HOUR),
        )

        val flagged = lateArrivalTimesByKey(messages, received, OWN)

        assertEquals(mapOf(messageStableKey(messages[0]) to 3 * HOUR), flagged)
    }

    @Test
    fun `an empty conversation is handled without touching the core`() {
        assertTrue(lateArrivalTimesByKey(emptyList(), emptyList(), OWN).isEmpty())
    }
}

class ChatScrollLogicInsertTest {
    private val tail = "tail"

    @Test
    fun `a delayed message above the tail shows the chip even at the bottom`() {
        val decision = ChatScrollLogic.decide(
            previousNewestKey = tail,
            currentNewestKey = tail,
            firstVisibleItemIndex = 0,
            isNewestOwnMessage = false,
            insertedAboveTail = true,
        )

        assertEquals(ChatScrollLogic.Decision.SHOW_INSERTED_ABOVE_CHIP, decision)
    }

    @Test
    fun `history backfill still leaves the reader alone`() {
        // New keys above the tail, but none of them flagged: FA7's case.
        val decision = ChatScrollLogic.decide(
            previousNewestKey = tail,
            currentNewestKey = tail,
            firstVisibleItemIndex = 12,
            isNewestOwnMessage = false,
            insertedAboveTail = false,
        )

        assertEquals(ChatScrollLogic.Decision.NONE, decision)
    }

    @Test
    fun `a new message at the tail keeps its existing behaviour`() {
        assertEquals(
            ChatScrollLogic.Decision.AUTO_SCROLL,
            ChatScrollLogic.decide(tail, "newer", 0, false, insertedAboveTail = true),
        )
        assertEquals(
            ChatScrollLogic.Decision.SHOW_NEW_MESSAGES_CHIP,
            ChatScrollLogic.decide(tail, "newer", 9, false, insertedAboveTail = false),
        )
    }

    @Test
    fun `the jump target is the oldest flagged insert`() {
        val previous = setOf("a", "d")
        val current = listOf("a", "b", "c", "d")

        assertEquals(
            1,
            ChatScrollLogic.oldestInsertedIndex(previous, current, lateArrivalKeys = setOf("b", "c")),
        )
        // Only "c" is flagged, so "b" (plain backfill) is not the target.
        assertEquals(
            2,
            ChatScrollLogic.oldestInsertedIndex(previous, current, lateArrivalKeys = setOf("c")),
        )
    }

    @Test
    fun `nothing to jump to on first load or with no flagged inserts`() {
        assertEquals(
            null,
            ChatScrollLogic.oldestInsertedIndex(emptySet(), listOf("a", "b"), setOf("a", "b")),
        )
        assertEquals(
            null,
            ChatScrollLogic.oldestInsertedIndex(setOf("a"), listOf("a", "b"), emptySet()),
        )
    }
}
