package com.cruisemesh.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.TimeZone

class ConversationLayoutTest {

    private val utc = TimeZone.getTimeZone("UTC")

    @Test
    fun `same sender within five minutes joins into one group`() {
        val messages = listOf(
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_608_000_000L),
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_608_180_000L),
        )

        val first = bubbleGroupingFor(messages, index = 0, timeZone = utc)
        val second = bubbleGroupingFor(messages, index = 1, timeZone = utc)

        assertTrue(first.joinsNext)
        assertFalse(first.showTimestamp)
        assertTrue(second.joinsPrevious)
        assertTrue(second.showTimestamp)
    }

    @Test
    fun `messages beyond the grouping window stay visually separate`() {
        val messages = listOf(
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_608_000_000L),
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_608_400_000L),
        )

        val first = bubbleGroupingFor(messages, index = 0, timeZone = utc)
        val second = bubbleGroupingFor(messages, index = 1, timeZone = utc)

        assertFalse(first.joinsNext)
        assertTrue(first.showTimestamp)
        assertFalse(second.joinsPrevious)
    }

    @Test
    fun `sender changes always break a group`() {
        val messages = listOf(
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_608_000_000L),
            ConversationMessageMeta(senderKey = "jules", timestampMs = 1_783_608_060_000L),
        )

        val first = bubbleGroupingFor(messages, index = 0, timeZone = utc)
        val second = bubbleGroupingFor(messages, index = 1, timeZone = utc)

        assertFalse(first.joinsNext)
        assertFalse(second.joinsPrevious)
    }

    @Test
    fun `crossing midnight breaks the group even inside five minutes`() {
        val messages = listOf(
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_727_940_000L),
            ConversationMessageMeta(senderKey = "maya", timestampMs = 1_783_728_180_000L),
        )

        val first = bubbleGroupingFor(messages, index = 0, timeZone = utc)
        val second = bubbleGroupingFor(messages, index = 1, timeZone = utc)

        assertFalse(first.joinsNext)
        assertFalse(second.joinsPrevious)
    }

    @Test
    fun `conversation timestamp uses signal style clock format`() {
        assertEquals("2:40 PM", formatConversationTimestamp(1_783_608_000_000L, timeZone = utc))
    }
}
