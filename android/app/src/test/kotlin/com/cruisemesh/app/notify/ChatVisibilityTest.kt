package com.cruisemesh.app.notify

import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class ChatVisibilityTest {

    private fun userId(byte: Int): ByteArray = ByteArray(16) { byte.toByte() }

    @Before
    fun clearState() = ChatVisibility.reset()

    @After
    fun clearStateAfter() = ChatVisibility.reset()

    @Test
    fun `nothing is visible initially`() {
        assertFalse(ChatVisibility.isVisible(userId(1)))
    }

    @Test
    fun `setVisible makes exactly that chat visible`() {
        ChatVisibility.setVisible(userId(1))
        assertTrue(ChatVisibility.isVisible(userId(1)))
        assertFalse(ChatVisibility.isVisible(userId(2)))
    }

    @Test
    fun `matching is by content not reference`() {
        ChatVisibility.setVisible(userId(1))
        // A distinct ByteArray instance with the same bytes must match --
        // callers pass ids from different sources (nav args, wire frames).
        assertTrue(ChatVisibility.isVisible(userId(1)))
    }

    @Test
    fun `mutating the caller's array after setVisible does not corrupt the registration`() {
        val id = userId(1)
        ChatVisibility.setVisible(id)
        id.fill(9)
        assertTrue(ChatVisibility.isVisible(userId(1)))
        assertFalse(ChatVisibility.isVisible(id))
    }

    @Test
    fun `clearVisible removes a matching registration`() {
        ChatVisibility.setVisible(userId(1))
        ChatVisibility.clearVisible(userId(1))
        assertFalse(ChatVisibility.isVisible(userId(1)))
    }

    @Test
    fun `clearVisible for a different chat is a no-op`() {
        // Screen transition ordering: chat B registers before chat A's
        // disposal runs. A's stale clear must not wipe B's registration.
        ChatVisibility.setVisible(userId(1))
        ChatVisibility.setVisible(userId(2))
        ChatVisibility.clearVisible(userId(1))
        assertTrue(ChatVisibility.isVisible(userId(2)))
    }

    @Test
    fun `setVisible replaces the previous chat`() {
        ChatVisibility.setVisible(userId(1))
        ChatVisibility.setVisible(userId(2))
        assertFalse(ChatVisibility.isVisible(userId(1)))
        assertTrue(ChatVisibility.isVisible(userId(2)))
    }

    @Test
    fun `clearVisible when nothing is visible is a no-op`() {
        ChatVisibility.clearVisible(userId(1))
        assertFalse(ChatVisibility.isVisible(userId(1)))
    }
}
