package com.cruisemesh.app.chat

/**
 * Pure placement math for [MessageFocusOverlay]
 * (MESSAGE_LONGPRESS_OVERLAY.md §5): keeps the reaction bar above and the
 * action menu below the focused bubble, sliding the whole stack to stay
 * on-screen, or pinning bar-to-top/menu-to-bottom when the stack is taller
 * than the viewport. Kept free of Compose/Android types so it's
 * unit-testable without a Compose host, same pattern as [ConversationLayout]
 * / `MeshRouterState`.
 */
object OverlayPlacement {

    data class Bounds(val left: Float, val top: Float, val right: Float, val bottom: Float) {
        val width: Float get() = right - left
        val height: Float get() = bottom - top
    }

    data class Result(
        val bubbleTop: Float,
        val barTop: Float,
        val menuTop: Float,
        val barLeft: Float,
        val menuLeft: Float,
    )

    fun compute(
        bubbleBounds: Bounds,
        barWidth: Float,
        barHeight: Float,
        menuWidth: Float,
        menuHeight: Float,
        screenTop: Float,
        screenBottom: Float,
        screenLeft: Float,
        screenRight: Float,
        spacing: Float,
        margin: Float,
        isOwn: Boolean,
    ): Result {
        val available = screenBottom - screenTop
        val totalHeight = barHeight + spacing + bubbleBounds.height + spacing + menuHeight

        val (bubbleTop, barTop, menuTop) = if (totalHeight > available) {
            // Rule 4: too tall to fit -- pin bar to top, menu to bottom, bubble
            // top-aligned under the bar (may extend under the menu).
            val pinnedBarTop = screenTop
            val pinnedMenuTop = screenBottom - menuHeight
            val pinnedBubbleTop = pinnedBarTop + barHeight + spacing
            Triple(pinnedBubbleTop, pinnedBarTop, pinnedMenuTop)
        } else {
            val naturalBarTop = bubbleBounds.top - spacing - barHeight
            val naturalMenuTop = bubbleBounds.bottom + spacing

            var shift = 0f
            // Rule 2: near top -- shift the whole stack down just enough.
            if (naturalBarTop + shift < screenTop) {
                shift += screenTop - (naturalBarTop + shift)
            }
            // Rule 3: near bottom -- shift the whole stack up just enough.
            val menuBottom = naturalMenuTop + shift + menuHeight
            if (menuBottom > screenBottom) {
                shift += screenBottom - menuBottom
            }
            Triple(bubbleBounds.top + shift, naturalBarTop + shift, naturalMenuTop + shift)
        }

        fun horizontalLeft(elementWidth: Float): Float {
            val raw = if (isOwn) bubbleBounds.right - elementWidth else bubbleBounds.left
            val minLeft = screenLeft + margin
            val maxLeft = (screenRight - margin - elementWidth).coerceAtLeast(minLeft)
            return raw.coerceIn(minLeft, maxLeft)
        }

        return Result(
            bubbleTop = bubbleTop,
            barTop = barTop,
            menuTop = menuTop,
            barLeft = horizontalLeft(barWidth),
            menuLeft = horizontalLeft(menuWidth),
        )
    }
}
