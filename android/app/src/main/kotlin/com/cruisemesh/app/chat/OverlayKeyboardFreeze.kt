package com.cruisemesh.app.chat

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
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
 * screen). Scaffold's content insets include the IME, so without
 * intervention the list (bottom-pinned via reverseLayout) and composer
 * re-layout every frame of the hide animation and chase the keyboard down
 * the screen — clearly visible through the half-transparent scrim, with the
 * overlay's bright bubble copy detaching from its dimmed original. Same
 * flicker in reverse when the overlay closes.
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
 * This version doesn't reconstruct anything: [trackLiveBottomInset] is fed
 * the *actual* `innerPadding.calculateBottomPadding()` Scaffold computed,
 * every recomposition, straight from the call site. [extraBottomPx] is
 * simply `captured − thatSameLiveNumber`, so caller adds it back with a
 * plain `.padding(bottom = ...)`: total = live (from Scaffold) + (captured −
 * live) = captured, always, by construction — there is no second source of
 * truth left to drift out of sync.
 *
 * The freeze releases itself once the live inset returns to the captured
 * height (a visual no-op, since the two now agree), or after
 * [releaseTimeoutMs] if it never returns (e.g. the composer lost focus while
 * the overlay was open).
 */
@Stable
class OverlayKeyboardFreeze internal constructor(
    private val keyboardController: SoftwareKeyboardController?,
    private val releaseTimeoutMs: Long = RELEASE_TIMEOUT_MS,
) {
    private var liveBottomPx by mutableFloatStateOf(0f)
    private var frozenBottomPx by mutableFloatStateOf(0f)

    /**
     * Call every recomposition of the Scaffold content lambda with
     * `innerPadding.calculateBottomPadding()` converted to px, so the freeze
     * always has Scaffold's real current bottom inset on hand to diff
     * against and to capture from.
     */
    fun trackLiveBottomInset(px: Float) {
        liveBottomPx = px
    }

    /**
     * Extra bottom padding to add on top of Scaffold's own (still-live)
     * inset so the total stays pinned at the height captured by
     * [onOverlayOpened]. Zero while not frozen.
     */
    val extraBottomPx: Float
        get() {
            val frozen = frozenBottomPx
            return if (frozen <= 0f) 0f else (frozen - liveBottomPx).coerceAtLeast(0f)
        }

    /** Call before showing the overlay: captures the current inset, then hides the keyboard. */
    fun onOverlayOpened() {
        frozenBottomPx = liveBottomPx
        keyboardController?.hide()
    }

    /**
     * Call after removing the overlay: brings the keyboard back, but only if
     * it was actually open at press time — an unconditional show() would pop
     * the keyboard open after a long-press in a keyboard-closed chat.
     */
    fun onOverlayClosed() {
        if (frozenBottomPx > 0f) {
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
        if (frozenBottomPx <= 0f) return
        withTimeoutOrNull(releaseTimeoutMs) {
            snapshotFlow { liveBottomPx >= frozenBottomPx }.first { it }
        }
        frozenBottomPx = 0f
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
