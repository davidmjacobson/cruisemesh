package com.cruisemesh.app.chat

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.ui.platform.SoftwareKeyboardController
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * JVM tests for [OverlayKeyboardFreeze]: the compensating inset must keep the
 * total bottom inset (live IME + freeze) constant from long-press until the
 * keyboard has fully returned, so the conversation never moves under the
 * overlay scrim. Uses hand-rolled fakes; no Compose host needed.
 */
class OverlayKeyboardFreezeTest {

    private val density = Density(1f)

    private class FakeImeInsets(var bottom: Int) : WindowInsets {
        override fun getBottom(density: Density) = bottom
        override fun getTop(density: Density) = 0
        override fun getLeft(density: Density, layoutDirection: LayoutDirection) = 0
        override fun getRight(density: Density, layoutDirection: LayoutDirection) = 0
    }

    private class FakeKeyboard : SoftwareKeyboardController {
        var hideCalls = 0
        var showCalls = 0
        override fun hide() { hideCalls++ }
        override fun show() { showCalls++ }
    }

    private fun freezeOf(
        keyboard: FakeKeyboard,
        ime: FakeImeInsets,
        releaseTimeoutMs: Long = OverlayKeyboardFreeze.RELEASE_TIMEOUT_MS,
    ) = OverlayKeyboardFreeze(keyboard, ime, density, releaseTimeoutMs)

    @Test
    fun `insets mirror the live ime while not frozen`() {
        val ime = FakeImeInsets(bottom = 300)
        val freeze = freezeOf(FakeKeyboard(), ime)
        assertEquals(300, freeze.insets.getBottom(density))
        ime.bottom = 120
        assertEquals(120, freeze.insets.getBottom(density))
    }

    @Test
    fun `open captures keyboard height and pins the inset there regardless of live ime`() {
        val keyboard = FakeKeyboard()
        val ime = FakeImeInsets(bottom = 300)
        val freeze = freezeOf(keyboard, ime)

        freeze.onOverlayOpened()

        assertEquals(1, keyboard.hideCalls)
        // Frozen: the inset stays pinned at the captured value no matter what
        // the live ime does mid-animation, at rest closed, or moving again.
        assertEquals(300, freeze.insets.getBottom(density))
        ime.bottom = 120
        assertEquals(300, freeze.insets.getBottom(density))
        ime.bottom = 0
        assertEquals(300, freeze.insets.getBottom(density))
    }

    @Test
    fun `open with keyboard closed is a no-op and close does not pop the keyboard`() {
        val keyboard = FakeKeyboard()
        val ime = FakeImeInsets(bottom = 0)
        val freeze = freezeOf(keyboard, ime)

        freeze.onOverlayOpened()
        assertEquals(0, freeze.insets.getBottom(density))

        freeze.onOverlayClosed()
        assertEquals(0, keyboard.showCalls)
    }

    @Test
    fun `close reshows the keyboard when it was open at press time`() {
        val keyboard = FakeKeyboard()
        val ime = FakeImeInsets(bottom = 300)
        val freeze = freezeOf(keyboard, ime)

        freeze.onOverlayOpened()
        ime.bottom = 0
        freeze.onOverlayClosed()

        assertEquals(1, keyboard.showCalls)
    }

    @Test
    fun `release is immediate when nothing is frozen`() = runBlocking {
        val freeze = freezeOf(FakeKeyboard(), FakeImeInsets(bottom = 0))
        freeze.releaseWhenKeyboardReturns()
        assertEquals(0, freeze.insets.getBottom(density))
    }

    @Test
    fun `release unfreezes once the keyboard is back at its captured height`() = runBlocking {
        val ime = FakeImeInsets(bottom = 300)
        val freeze = freezeOf(FakeKeyboard(), ime)

        freeze.onOverlayOpened()
        ime.bottom = 0
        ime.bottom = 300 // keyboard returned before release ran

        freeze.releaseWhenKeyboardReturns()
        // Unfrozen: no compensation left on top of the live inset.
        ime.bottom = 0
        assertEquals(0, freeze.insets.getBottom(density))
    }

    @Test
    fun `release falls back to the timeout when the keyboard never returns`() = runBlocking {
        val ime = FakeImeInsets(bottom = 300)
        val freeze = freezeOf(FakeKeyboard(), ime, releaseTimeoutMs = 50)

        freeze.onOverlayOpened()
        ime.bottom = 0
        assertEquals(300, freeze.insets.getBottom(density))

        freeze.releaseWhenKeyboardReturns()
        assertEquals(0, freeze.insets.getBottom(density))
    }
}
