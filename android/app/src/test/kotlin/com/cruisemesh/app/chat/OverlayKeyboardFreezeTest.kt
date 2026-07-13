package com.cruisemesh.app.chat

import androidx.compose.ui.platform.SoftwareKeyboardController
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * JVM tests for [OverlayKeyboardFreeze]: while frozen, `extraBottomPx` must
 * cancel any increase in the usable viewport edge supplied through
 * [OverlayKeyboardFreeze.trackLiveContentBottom]. Uses a hand-rolled
 * keyboard-controller fake; no Compose host needed.
 */
class OverlayKeyboardFreezeTest {

    private class FakeKeyboard : SoftwareKeyboardController {
        var hideCalls = 0
        var showCalls = 0
        override fun hide() { hideCalls++ }
        override fun show() { showCalls++ }
    }

    private fun freezeOf(
        keyboard: FakeKeyboard,
        releaseTimeoutMs: Long = OverlayKeyboardFreeze.RELEASE_TIMEOUT_MS,
    ) = OverlayKeyboardFreeze(keyboard, releaseTimeoutMs)

    @Test
    fun `extraBottomPx is zero while not frozen even with a live inset`() {
        val freeze = freezeOf(FakeKeyboard())
        freeze.trackLiveContentBottom(500f, imeVisible = true)
        assertEquals(0f, freeze.extraBottomPx)
    }

    @Test
    fun `open captures the content edge and padding cancels viewport growth`() {
        val keyboard = FakeKeyboard()
        val freeze = freezeOf(keyboard)
        var live = 500f
        freeze.trackLiveContentBottom(live, imeVisible = true)

        freeze.onOverlayOpened()

        assertEquals(1, keyboard.hideCalls)
        // Frozen: usable edge minus compensating bottom padding stays at the
        // captured keyboard-open position throughout the resize animation.
        live = 500f; freeze.trackLiveContentBottom(live, imeVisible = true)
        assertEquals(500f, live - freeze.extraBottomPx)
        live = 650f; freeze.trackLiveContentBottom(live, imeVisible = true)
        assertEquals(500f, live - freeze.extraBottomPx)
        live = 800f; freeze.trackLiveContentBottom(live, imeVisible = false)
        assertEquals(500f, live - freeze.extraBottomPx)
    }

    @Test
    fun `open with navigation inset but no IME is a no-op and close does not pop the keyboard`() {
        val keyboard = FakeKeyboard()
        val freeze = freezeOf(keyboard)
        freeze.trackLiveContentBottom(800f, imeVisible = false)

        freeze.onOverlayOpened()
        assertEquals(0f, freeze.extraBottomPx)
        assertEquals(0, keyboard.hideCalls)

        freeze.onOverlayClosed()
        assertEquals(0, keyboard.showCalls)
    }

    @Test
    fun `close reshows the keyboard when it was open at press time`() {
        val keyboard = FakeKeyboard()
        val freeze = freezeOf(keyboard)
        freeze.trackLiveContentBottom(500f, imeVisible = true)

        freeze.onOverlayOpened()
        freeze.trackLiveContentBottom(800f, imeVisible = false)
        freeze.onOverlayClosed()

        assertEquals(1, keyboard.showCalls)
    }

    @Test
    fun `release is immediate when nothing is frozen`() = runBlocking {
        val freeze = freezeOf(FakeKeyboard())
        freeze.releaseWhenKeyboardReturns()
        assertEquals(0f, freeze.extraBottomPx)
    }

    @Test
    fun `release unfreezes once the live inset is back at its captured height`() = runBlocking {
        val freeze = freezeOf(FakeKeyboard())
        freeze.trackLiveContentBottom(500f, imeVisible = true)

        freeze.onOverlayOpened()
        freeze.trackLiveContentBottom(800f, imeVisible = false)
        freeze.trackLiveContentBottom(500f, imeVisible = true) // keyboard returned before release ran

        freeze.releaseWhenKeyboardReturns()
        // Unfrozen: no compensation left on top of the live inset.
        freeze.trackLiveContentBottom(800f, imeVisible = false)
        assertEquals(0f, freeze.extraBottomPx)
    }

    @Test
    fun `release falls back to the timeout when the live inset never returns`() = runBlocking {
        val freeze = freezeOf(FakeKeyboard(), releaseTimeoutMs = 50)
        freeze.trackLiveContentBottom(500f, imeVisible = true)

        freeze.onOverlayOpened()
        freeze.trackLiveContentBottom(800f, imeVisible = false)
        assertEquals(300f, freeze.extraBottomPx)

        freeze.releaseWhenKeyboardReturns()
        assertEquals(0f, freeze.extraBottomPx)
    }
}
