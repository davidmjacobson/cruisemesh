package com.cruisemesh.app.chat

import androidx.compose.ui.platform.SoftwareKeyboardController
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * JVM tests for [OverlayKeyboardFreeze]: while frozen, `extraBottomPx` plus
 * whatever the caller feeds in via [OverlayKeyboardFreeze.trackLiveBottomInset]
 * (standing in for Scaffold's own live bottom inset) must always sum to
 * exactly the value captured at [OverlayKeyboardFreeze.onOverlayOpened] --
 * both numbers come from the same tracked source, so there's nothing left to
 * drift apart. Uses a hand-rolled keyboard-controller fake; no Compose host
 * needed.
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
        freeze.trackLiveBottomInset(300f)
        assertEquals(0f, freeze.extraBottomPx)
    }

    @Test
    fun `open captures the live inset and total stays pinned as it drops to zero`() {
        val keyboard = FakeKeyboard()
        val freeze = freezeOf(keyboard)
        var live = 300f
        freeze.trackLiveBottomInset(live)

        freeze.onOverlayOpened()

        assertEquals(1, keyboard.hideCalls)
        // Frozen: live + extra must sum to the captured 300 at every point
        // along the animation, including the tail where the live source
        // (Scaffold's real inset) does something nonlinear -- it doesn't
        // matter here since we never re-derive it, just diff against it.
        live = 300f; freeze.trackLiveBottomInset(live)
        assertEquals(300f, live + freeze.extraBottomPx)
        live = 48f; freeze.trackLiveBottomInset(live) // e.g. floored at navigationBars height
        assertEquals(300f, live + freeze.extraBottomPx)
        live = 0f; freeze.trackLiveBottomInset(live)
        assertEquals(300f, live + freeze.extraBottomPx)
    }

    @Test
    fun `open with no live inset is a no-op and close does not pop the keyboard`() {
        val keyboard = FakeKeyboard()
        val freeze = freezeOf(keyboard)
        freeze.trackLiveBottomInset(0f)

        freeze.onOverlayOpened()
        assertEquals(0f, freeze.extraBottomPx)

        freeze.onOverlayClosed()
        assertEquals(0, keyboard.showCalls)
    }

    @Test
    fun `close reshows the keyboard when it was open at press time`() {
        val keyboard = FakeKeyboard()
        val freeze = freezeOf(keyboard)
        freeze.trackLiveBottomInset(300f)

        freeze.onOverlayOpened()
        freeze.trackLiveBottomInset(0f)
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
        freeze.trackLiveBottomInset(300f)

        freeze.onOverlayOpened()
        freeze.trackLiveBottomInset(0f)
        freeze.trackLiveBottomInset(300f) // keyboard returned before release ran

        freeze.releaseWhenKeyboardReturns()
        // Unfrozen: no compensation left on top of the live inset.
        freeze.trackLiveBottomInset(0f)
        assertEquals(0f, freeze.extraBottomPx)
    }

    @Test
    fun `release falls back to the timeout when the live inset never returns`() = runBlocking {
        val freeze = freezeOf(FakeKeyboard(), releaseTimeoutMs = 50)
        freeze.trackLiveBottomInset(300f)

        freeze.onOverlayOpened()
        freeze.trackLiveBottomInset(0f)
        assertEquals(300f, freeze.extraBottomPx)

        freeze.releaseWhenKeyboardReturns()
        assertEquals(0f, freeze.extraBottomPx)
    }
}
