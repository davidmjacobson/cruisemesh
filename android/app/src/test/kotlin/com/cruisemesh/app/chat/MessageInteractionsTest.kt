package com.cruisemesh.app.chat

import com.cruisemesh.app.media.KIND_REACTION
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.StoredMessage

class MessageInteractionsTest {
    private val ownUserId = byteArrayOf(0x01)
    private val peerUserId = byteArrayOf(0x02)
    private val chatId = byteArrayOf(0x09)

    @Test
    fun reactionPayloadRoundTripsTargetReference() {
        val target = MessageTarget(peerUserId, 7uL, KIND_TEXT)
        val payload = ReactionPayload(target, "👍")
        val decoded = ReactionPayload.decode(payload.encode())

        assertEquals(target, decoded?.target)
        assertEquals("👍", decoded?.emoji)
    }

    @Test
    fun latestReactionPerUserWinsAndBlankClears() {
        val target = MessageTarget(peerUserId, 1uL, KIND_TEXT)
        val messages = listOf(
            text(peerUserId, 1uL, "hello"),
            reaction(ownUserId, 1uL, target, "👍"),
            reaction(peerUserId, 2uL, target, "❤️"),
            reaction(ownUserId, 3uL, target, "😂"),
            reaction(peerUserId, 4uL, target, ""),
        )

        val summaries = reactionSummariesByTarget(messages, ownUserId)[target.stableKey]

        assertEquals(listOf(ReactionSummary("😂", 1, true)), summaries)
    }

    @Test
    fun hiddenReactionDoesNotCreateVisibleGap() {
        val messages = listOf(
            text(peerUserId, 1uL, "one"),
            reaction(peerUserId, 2uL, MessageTarget(ownUserId, 1uL, KIND_TEXT), "👍"),
            text(peerUserId, 3uL, "three"),
        )
        val visible = messages.filter { it.kind == KIND_TEXT }

        assertTrue(visibleGapIndices(messages, visible).isEmpty())
    }

    private fun text(sender: ByteArray, lamport: ULong, body: String): StoredMessage =
        StoredMessage(chatId, sender, lamport, lamport.toLong(), KIND_TEXT, body.toByteArray())

    private fun reaction(
        sender: ByteArray,
        lamport: ULong,
        target: MessageTarget,
        emoji: String,
    ): StoredMessage =
        StoredMessage(chatId, sender, lamport, lamport.toLong(), KIND_REACTION, ReactionPayload(target, emoji).encode())

    private companion object {
        val KIND_TEXT: UByte = 1u
    }
}
