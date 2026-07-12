package com.cruisemesh.app.chat

/**
 * Pure placement math for [MessageFocusOverlay]
 * (MESSAGE_LONGPRESS_OVERLAY.md §5): keeps the reaction bar above and the
 * action menu below the focused bubble when there is room. The bubble itself
 * stays anchored at the long-press position; when an edge is tight, the bar
 * and menu move around that fixed anchor. Kept free of Compose/Android types
 * so it's unit-testable without a Compose host, same pattern as
 * [ConversationLayout] / `MeshRouterState`.
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
        val bubbleTop = bubbleBounds.top
        val naturalBarTop = bubbleBounds.top - spacing - barHeight
        val naturalMenuTop = bubbleBounds.bottom + spacing
        val barFitsAbove = naturalBarTop >= screenTop
        val menuFitsBelow = naturalMenuTop + menuHeight <= screenBottom

        var barTop = if (barFitsAbove) {
            naturalBarTop
        } else {
            naturalMenuTop
        }

        var menuTop = if (menuFitsBelow) {
            naturalMenuTop
        } else {
            bubbleBounds.top - spacing - menuHeight
        }

        val overlap = barTop < menuTop + menuHeight && menuTop < barTop + barHeight
        if (overlap) {
            if (barTop >= bubbleBounds.bottom) {
                menuTop = barTop + barHeight + spacing
            } else {
                barTop = menuTop - spacing - barHeight
            }
        }

        fun clampVerticalTop(value: Float, elementHeight: Float): Float {
            val maxTop = (screenBottom - elementHeight).coerceAtLeast(screenTop)
            return value.coerceIn(screenTop, maxTop)
        }

        barTop = clampVerticalTop(barTop, barHeight)
        menuTop = clampVerticalTop(menuTop, menuHeight)

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
