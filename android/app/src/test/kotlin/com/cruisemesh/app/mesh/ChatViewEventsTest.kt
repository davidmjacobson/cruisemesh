package com.cruisemesh.app.mesh

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Test

class ChatViewEventsTest {

    private fun userId(byte: Int): ByteArray = ByteArray(16) { byte.toByte() }

    @After
    fun clearRegistration() = ChatViewEvents.unregister()

    @Test
    fun `onChatViewed with nothing registered is a no-op`() {
        // Must not throw -- e.g. the mesh isn't running yet.
        ChatViewEvents.onChatViewed(userId(1))
    }

    @Test
    fun `a registered handler receives the viewed chatId`() {
        var received: ByteArray? = null
        ChatViewEvents.register { received = it }

        ChatViewEvents.onChatViewed(userId(1))

        assertEquals(userId(1).toList(), received!!.toList())
    }

    @Test
    fun `unregister stops delivering to the old handler`() {
        var callCount = 0
        ChatViewEvents.register { callCount++ }
        ChatViewEvents.unregister()

        ChatViewEvents.onChatViewed(userId(1))

        assertEquals(0, callCount)
    }

    @Test
    fun `registering again replaces the previous handler`() {
        var firstCalls = 0
        var secondCalls = 0
        ChatViewEvents.register { firstCalls++ }
        ChatViewEvents.register { secondCalls++ }

        ChatViewEvents.onChatViewed(userId(1))

        assertEquals(0, firstCalls)
        assertEquals(1, secondCalls)
    }
}
