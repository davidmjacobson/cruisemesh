package com.cruisemesh.app.chat

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.ime
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.SoftwareKeyboardController
import androidx.compose.ui.unit.Density
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Keeps the conversation layout pixel-frozen while [MessageFocusOverlay] is
 * open, so opening it looks like Signal: the pressed bubble stays exactly
 * where it is while the background dims and the keyboard slides away.
 *
 * Opening the overlay hides the soft keyboard (the scrim owns the whole
 * screen). The chat Scaffold's content insets normally include the IME, so
 * without intervention the list (bottom-pinned via reverseLayout) and
 * composer re-layout every frame of the hide animation and chase the
 * keyboard down the screen — clearly visible through the half-transparent
 * scrim, with the overlay's bright bubble copy detaching from its dimmed
 * original. Same flicker in reverse when the overlay closes.
 *
 * [insets] is meant to fully *own* IME padding in place of Scaffold's own
 * (the caller passes `contentWindowInsets = WindowInsets.safeDrawing.exclude
 * (WindowInsets.ime)` to Scaffold and applies [insets] itself below
 * `innerPadding`). It is a plain on/off switch rather than a live diff: while
 * not frozen it mirrors the live IME inset (identical to `imePadding()`);
 * once frozen it returns a *fixed* inset pinned at the height captured when
 * the overlay opened, completely ignoring the live IME value until released.
 * An earlier version tried to keep the total inset constant by subtracting
 * the live IME height from the captured one, layered on top of Scaffold's
 * own (still-live) IME padding — two independently animating numbers that
 * were supposed to cancel out. They didn't: Scaffold's default
 * `contentWindowInsets` is `safeDrawing`, which unions IME with
 * `navigationBars` (a max, not IME alone), so it stops shrinking once IME
 * drops below the nav-bar height while the raw IME reading kept going to
 * zero — the two sources drifted apart and the composer visibly detached
 * from the overlay. A single fixed value while frozen has nothing to drift
 * against.
 *
 * The freeze releases itself once the returning keyboard reaches its
 * captured height (a visual no-op, since live and frozen now agree), or
 * after [releaseTimeoutMs] if it never returns (e.g. the composer lost focus
 * while the overlay was open).
 */
@Stable
class OverlayKeyboardFreeze internal constructor(
    private val keyboardController: SoftwareKeyboardController?,
    private val imeInsets: WindowInsets,
    private val density: Density,
    private val releaseTimeoutMs: Long = RELEASE_TIMEOUT_MS,
) {
    private var frozenImeBottomPx by mutableStateOf(0)

    /**
     * While frozen: a fixed bottom inset pinned at the height captured by
     * [onOverlayOpened], independent of the live IME value. While not
     * frozen: mirrors the live IME inset, i.e. behaves exactly like
     * `WindowInsets.ime` / `imePadding()`.
     */
    val insets: WindowInsets
        get() {
            val frozen = frozenImeBottomPx
            return if (frozen > 0) WindowInsets(bottom = frozen) else imeInsets
        }

    /** Call before showing the overlay: captures the keyboard height, then hides it. */
    fun onOverlayOpened() {
        frozenImeBottomPx = imeInsets.getBottom(density)
        keyboardController?.hide()
    }

    /**
     * Call after removing the overlay: brings the keyboard back, but only if
     * it was actually open at press time — the old unconditional show() could
     * pop the keyboard open after a long-press in a keyboard-closed chat.
     */
    fun onOverlayClosed() {
        if (frozenImeBottomPx > 0) {
            keyboardController?.show()
        }
    }

    /**
     * Suspends until the returning keyboard reaches the captured height (so
     * dropping the freeze changes nothing visually), then unfreezes. Falls
     * back to unfreezing after [releaseTimeoutMs] if the keyboard never
     * comes back. Run from a LaunchedEffect keyed on the overlay closing.
     */
    suspend fun releaseWhenKeyboardReturns() {
        if (frozenImeBottomPx == 0) return
        withTimeoutOrNull(releaseTimeoutMs) {
            snapshotFlow { imeInsets.getBottom(density) >= frozenImeBottomPx }.first { it }
        }
        frozenImeBottomPx = 0
    }

    companion object {
        /** Comfortably longer than the IME slide-in animation (~300 ms). */
        const val RELEASE_TIMEOUT_MS = 1_000L
    }
}

@Composable
fun rememberOverlayKeyboardFreeze(): OverlayKeyboardFreeze {
    val keyboardController = LocalSoftwareKeyboardController.current
    val imeInsets = WindowInsets.ime
    val density = LocalDensity.current
    return remember(keyboardController, imeInsets, density) {
        OverlayKeyboardFreeze(keyboardController, imeInsets, density)
    }
}
