package com.cruisemesh.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.StoredMessage
import java.util.Calendar

class ChatListLogicTest {

    @Test
    fun testInitials() {
        val (_, init1) = ChatListLogic.avatarHueAndInitials(byteArrayOf(), "Alice", "CM-ABCD")
        assertEquals("AL", init1)
        
        val (_, init2) = ChatListLogic.avatarHueAndInitials(byteArrayOf(), "A", "CM-ABCD")
        assertEquals("A", init2)
        
        val (_, init3) = ChatListLogic.avatarHueAndInitials(byteArrayOf(), "", "CM-ABCD")
        assertEquals("AB", init3)
        
        val (_, init4) = ChatListLogic.avatarHueAndInitials(byteArrayOf(), "Unknown", "CM-1234")
        assertEquals("12", init4)
    }

    @Test
    fun testFormatRelativeTime() {
        val now = Calendar.getInstance()
        now.set(2026, Calendar.JULY, 9, 14, 0, 0)
        val nowMs = now.timeInMillis
        
        val sameDay = Calendar.getInstance()
        sameDay.set(2026, Calendar.JULY, 9, 9, 30, 0)
        assertEquals("9:30 AM", ChatListLogic.formatRelativeTime(sameDay.timeInMillis, nowMs))
        
        val twoDaysAgo = Calendar.getInstance()
        twoDaysAgo.set(2026, Calendar.JULY, 7, 14, 0, 0)
        assertEquals("Tue", ChatListLogic.formatRelativeTime(twoDaysAgo.timeInMillis, nowMs))
        
        val older = Calendar.getInstance()
        older.set(2026, Calendar.JUNE, 1, 14, 0, 0)
        assertEquals("Jun 1", ChatListLogic.formatRelativeTime(older.timeInMillis, nowMs))
    }

    @Test
    fun testComputeUnread() {
        val ownId = byteArrayOf(1)
        val peerId = byteArrayOf(2)
        val messages = listOf(
            StoredMessage(peerId, peerId, 1uL, 1000L, 1u.toUByte(), byteArrayOf()), // read
            StoredMessage(ownId, ownId, 2uL, 2000L, 1u.toUByte(), byteArrayOf()), // own message
            StoredMessage(peerId, peerId, 3uL, 3000L, 1u.toUByte(), byteArrayOf()), // unread
            StoredMessage(peerId, peerId, 4uL, 4000L, 1u.toUByte(), byteArrayOf()), // unread
            // Hidden friend-request stream noise must not inflate the badge.
            StoredMessage(peerId, peerId, 5uL, 5000L, 3u.toUByte(), byteArrayOf()),
        )
        val unread = ChatListLogic.computeUnread(messages, ownId, readThrough = 1uL)
        assertEquals(2, unread)
    }

    @Test
    fun lastVisibleMessageSkipsFriendRequests() {
        val peerId = byteArrayOf(2)
        val messages = listOf(
            StoredMessage(peerId, peerId, 1uL, 1000L, 1u.toUByte(), "hello".toByteArray()),
            StoredMessage(peerId, peerId, 2uL, 2000L, 3u.toUByte(), "{}".toByteArray()),
        )
        val last = ChatListLogic.lastVisibleMessage(messages)
        assertEquals(1uL, last!!.lamport)
        assertEquals("hello", ChatListLogic.previewText(last))
    }
}
