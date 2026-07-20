package com.cruisemesh.app.notify

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

class NotificationRequestCodesTest {

    @Test
    fun `the same chat and purpose always returns the same code`() {
        val codes = NotificationRequestCodes()
        val first = codes.requestCodeFor("aa11", "content")
        val second = codes.requestCodeFor("aa11", "content")
        assertEquals(first, second)
    }

    @Test
    fun `different purposes for the same chat never collide`() {
        val codes = NotificationRequestCodes()
        val content = codes.requestCodeFor("aa11", "content")
        val reply = codes.requestCodeFor("aa11", "com.cruisemesh.app.action.REPLY")
        val markRead = codes.requestCodeFor("aa11", "com.cruisemesh.app.action.MARK_READ")
        assertNotEquals(content, reply)
        assertNotEquals(content, markRead)
        assertNotEquals(reply, markRead)
    }

    @Test
    fun `different chats never collide, even with the same purpose`() {
        val codes = NotificationRequestCodes()
        val chatA = codes.requestCodeFor("aa11", "content")
        val chatB = codes.requestCodeFor("bb22", "content")
        assertNotEquals(chatA, chatB)
    }

    @Test
    fun `a large number of distinct chat-purpose pairs are all pairwise distinct`() {
        // Regression for the bug this replaces: ByteArray.contentHashCode()
        // is an Int hash that CAN collide between two different 32-byte
        // userIds. This allocator must not, no matter how many chats
        // accumulate over a session.
        val codes = NotificationRequestCodes()
        val seen = mutableSetOf<Int>()
        for (chat in 0 until 500) {
            for (purpose in listOf("content", "reply", "markRead")) {
                val code = codes.requestCodeFor("chat$chat", purpose)
                assertEquals("duplicate request code for chat$chat/$purpose", true, seen.add(code))
            }
        }
        assertEquals(1500, seen.size)
    }
}
