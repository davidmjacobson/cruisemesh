package com.cruisemesh.app.chat

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.SoftwareKeyboardController
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Keeps the conversation layout pixel-frozen while [MessageFocusOverlay] is
 * open, so opening it looks like Signal: the pressed bubble stays exactly
 * where it is while the background dims and the keyboard slides away.
 *
 * Opening the overlay hides the soft keyboard (the scrim owns the whole
 * screen). Android's adjust-resize behavior changes the Compose viewport as
 * the IME moves. Without intervention the list (bottom-pinned via
 * reverseLayout) and composer re-layout every frame of the hide animation
 * and chase the keyboard down the screen — clearly visible through the
 * half-transparent scrim, with the overlay's bright bubble copy detaching
 * from its dimmed original. Same flicker in reverse when the overlay closes.
 *
 * Two earlier versions of this tried to independently *recompute* what
 * Scaffold's bottom inset was doing (first by subtracting a live
 * `WindowInsets.ime` reading from a captured one, then by subtracting a raw
 * IME reading from Scaffold's own now-IME-excluded inset) and layer a
 * correction on top. Both failed because they mixed two separately-animating
 * numbers that were supposed to cancel exactly, and didn't quite —
 * `WindowInsets.safeDrawing` unions IME with `navigationBars` (a max, not a
 * sum), so a raw IME reading and Scaffold's actual applied inset part ways
 * for the last ~48px of the close animation.
 *
 * [trackLiveContentBottom] is fed the viewport's usable bottom edge (viewport
 * height minus Scaffold's bottom system-bar padding). While frozen,
 * [extraBottomPx] adds back exactly the amount by which that edge has moved
 * down: `liveContentBottom - capturedContentBottom`. This keeps the content
 * at its keyboard-open position without applying an IME inset on top of an
 * already-resized viewport. IME visibility is tracked separately so the
 * navigation bar is not mistaken for an open keyboard.
 *
 * The freeze releases itself once the returning keyboard restores the
 * captured content edge (a visual no-op, since the two now agree), or after
 * [releaseTimeoutMs] if it never returns (e.g. the composer lost focus while
 * the overlay was open).
 */
@Stable
class OverlayKeyboardFreeze internal constructor(
    private val keyboardController: SoftwareKeyboardController?,
    private val releaseTimeoutMs: Long = RELEASE_TIMEOUT_MS,
) {
    private var liveContentBottomPx by mutableFloatStateOf(0f)
    private var liveImeVisible by mutableStateOf(false)
    private var frozenContentBottomPx by mutableFloatStateOf(0f)
    private var restoreKeyboard by mutableStateOf(false)

    /**
     * Call every recomposition with the viewport's usable bottom edge in px:
     * viewport height minus Scaffold's current bottom system-bar padding.
     */
    fun trackLiveContentBottom(px: Float, imeVisible: Boolean) {
        liveContentBottomPx = px
        liveImeVisible = imeVisible
    }

    /**
     * Extra bottom padding that keeps the usable content edge pinned at the
     * position captured by
     * [onOverlayOpened]. Zero while not frozen.
     */
    val extraBottomPx: Float
        get() {
            val frozen = frozenContentBottomPx
            return if (frozen <= 0f) 0f else (liveContentBottomPx - frozen).coerceAtLeast(0f)
        }

    /** Call before showing the overlay: captures the content edge, then hides the keyboard. */
    fun onOverlayOpened() {
        restoreKeyboard = liveImeVisible
        frozenContentBottomPx = if (restoreKeyboard) liveContentBottomPx else 0f
        if (restoreKeyboard) {
            keyboardController?.hide()
        }
    }

    /**
     * Call after removing the overlay: brings the keyboard back, but only if
     * it was actually open at press time — an unconditional show() would pop
     * the keyboard open after a long-press in a keyboard-closed chat.
     */
    fun onOverlayClosed() {
        if (restoreKeyboard) {
            keyboardController?.show()
        }
    }

    /**
     * Suspends until the returning keyboard's live inset reaches the
     * captured height (so dropping the freeze changes nothing visually),
     * then unfreezes. Falls back to unfreezing after [releaseTimeoutMs] if
     * the keyboard never comes back. Run from a LaunchedEffect keyed on the
     * overlay closing.
     */
    suspend fun releaseWhenKeyboardReturns() {
        if (frozenContentBottomPx <= 0f) {
            restoreKeyboard = false
            return
        }
        withTimeoutOrNull(releaseTimeoutMs) {
            snapshotFlow {
                liveImeVisible && liveContentBottomPx <= frozenContentBottomPx
            }.first { it }
        }
        frozenContentBottomPx = 0f
        restoreKeyboard = false
    }

    companion object {
        /** Comfortably longer than the IME slide-in animation (~300 ms). */
        const val RELEASE_TIMEOUT_MS = 1_000L
    }
}

@Composable
fun rememberOverlayKeyboardFreeze(): OverlayKeyboardFreeze {
    val keyboardController = LocalSoftwareKeyboardController.current
    return remember(keyboardController) {
        OverlayKeyboardFreeze(keyboardController)
    }
}
