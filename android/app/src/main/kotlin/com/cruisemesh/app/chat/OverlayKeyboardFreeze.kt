package com.cruisemesh.app.chat

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.exclude
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
 * screen). The chat Scaffold's content insets include the IME, so without
 * intervention the list (bottom-pinned via reverseLayout) and composer
 * re-layout every frame of the hide animation and chase the keyboard down the
 * screen — clearly visible through the half-transparent scrim, with the
 * overlay's bright bubble copy detaching from its dimmed original. Same
 * flicker in reverse when the overlay closes and the keyboard comes back.
 *
 * [insets], applied below the Scaffold's own inset padding, freezes that:
 * while frozen it pads the bottom by (captured IME height − live IME height),
 * so the total bottom inset stays constant as the keyboard animates out, sits
 * closed, and animates back in. The freeze releases itself once the returning
 * keyboard reaches its captured height (a visual no-op), or after
 * [releaseTimeoutMs] if it never returns (e.g. the composer lost focus while
 * the overlay was open).
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
     * Extra bottom insets that top the live IME inset back up to the height
     * captured at [onOverlayOpened]. Zero (a no-op) while not frozen.
     */
    val insets: WindowInsets
        get() = WindowInsets(bottom = frozenImeBottomPx).exclude(imeInsets)

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
