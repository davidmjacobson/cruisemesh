package com.cruisemesh.app.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DraftChangeSignalTest {
    @Test
    fun sameEmptinessDoesNotNotify() {
        // Same-presence saves -- the reload storm this class exists to kill.
        assertFalse(DraftChangeSignal.shouldNotify("hello", "hello there"))
        assertFalse(DraftChangeSignal.shouldNotify("hello there", "hello"))
        assertFalse(DraftChangeSignal.shouldNotify("", ""))
        assertFalse(DraftChangeSignal.shouldNotify("\n\r", ""))
    }

    @Test
    fun emptyToNonEmptyNotifies() {
        assertTrue(DraftChangeSignal.shouldNotify("", "h"))
        assertTrue(DraftChangeSignal.shouldNotify("\n\r", "h"))
    }

    @Test
    fun nonEmptyToEmptyNotifies() {
        assertTrue(DraftChangeSignal.shouldNotify("hello", ""))
        assertTrue(DraftChangeSignal.shouldNotify("hello", "\n\r"))
    }

    @Test
    fun typingAKeystrokeAtATimeNotifiesOnlyOnce() {
        var previous = ""
        var notifyCount = 0
        for (next in listOf("h", "he", "hel", "hell", "hello")) {
            if (DraftChangeSignal.shouldNotify(previous, next)) notifyCount++
            previous = next
        }
        assertTrue("expected exactly one notify (the empty -> non-empty transition)", notifyCount == 1)
    }
}
