package com.cruisemesh.app.chat

/**
 * Pure placement math for [MessageFocusOverlay]:
 * keeps the reaction bar above and the action menu below the focused bubble.
 * When that complete stack fits in the viewport, the bubble moves just far
 * enough to make room at an edge -- the defining Signal interaction. Very
 * tall messages fall back to placing the controls around a fixed anchor.
 * Kept free of Compose/Android types
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
        // The bubble can be taller than the viewport or partly scrolled past an
        // edge, so free space is measured around its on-screen portion.
        val bubbleHeight = bubbleBounds.height.coerceAtLeast(0f)
        val viewportHeight = (screenBottom - screenTop).coerceAtLeast(0f)
        val barNeed = barHeight + spacing
        val menuNeed = menuHeight + spacing
        val completeStackHeight = barNeed + bubbleHeight + menuNeed

        var bubbleTop = bubbleBounds.top
        var barTop: Float
        var menuTop: Float

        if (completeStackHeight <= viewportHeight) {
            // Signal keeps the three pieces in their natural order and moves
            // the pressed message preview. At the bottom this visibly lifts
            // the bubble until the whole menu clears the navigation inset;
            // the corresponding top-edge case moves it down.
            val minBubbleTop = screenTop + barHeight + spacing
            val maxBubbleTop = screenBottom - menuHeight - spacing - bubbleHeight
            bubbleTop = bubbleBounds.top.coerceIn(minBubbleTop, maxBubbleTop)
            barTop = bubbleTop - spacing - barHeight
            menuTop = bubbleTop + bubbleHeight + spacing
        } else {
            val anchorTop = bubbleBounds.top.coerceIn(screenTop, screenBottom)
            val anchorBottom = bubbleBounds.bottom.coerceIn(screenTop, screenBottom)
            val aboveRoom = anchorTop - screenTop
            val belowRoom = screenBottom - anchorBottom
            val stackNeed = barNeed + menuNeed
            val barFitsAbove = aboveRoom >= barNeed
            val menuFitsBelow = belowRoom >= menuNeed

            when {
                // Natural: bar above the bubble, menu below it.
                barFitsAbove && menuFitsBelow -> {
                    barTop = anchorTop - barNeed
                    menuTop = anchorBottom + spacing
                }
                // Menu can't go below: stack bar-then-menu above when both fit...
                barFitsAbove && aboveRoom >= stackNeed -> {
                    menuTop = anchorTop - menuNeed
                    barTop = menuTop - barNeed
                }
                // ...or split them around the bubble (menu above, bar below).
                barFitsAbove && aboveRoom >= menuNeed && belowRoom >= barNeed -> {
                    menuTop = anchorTop - menuNeed
                    barTop = anchorBottom + spacing
                }
                // Menu fits nowhere around the bubble: bar keeps its natural spot,
                // menu pins to the bottom edge over the bubble.
                barFitsAbove -> {
                    barTop = anchorTop - barNeed
                    menuTop = screenBottom - menuHeight
                }
                // Bar can't go above: stack bar-then-menu below when both fit.
                menuFitsBelow && belowRoom >= stackNeed -> {
                    barTop = anchorBottom + spacing
                    menuTop = barTop + barNeed
                }
                // Menu keeps its natural spot below; bar pins to the top edge over the bubble.
                menuFitsBelow -> {
                    barTop = screenTop
                    menuTop = anchorBottom + spacing
                }
                // No room on either side (bubble fills the viewport): pin both over
                // the bubble at opposite edges.
                else -> {
                    barTop = screenTop
                    menuTop = screenBottom - menuHeight
                }
            }
        }

        // Safety net for viewports smaller than the controls themselves: stay
        // on-screen, and never overlap each other -- overlapping controls are
        // worse than controls crowding the bubble, so the overlap check runs
        // last (a clamp can push an element back onto the other).
        fun clampVerticalTop(value: Float, elementHeight: Float): Float {
            val maxTop = (screenBottom - elementHeight).coerceAtLeast(screenTop)
            return value.coerceIn(screenTop, maxTop)
        }
        barTop = clampVerticalTop(barTop, barHeight)
        menuTop = clampVerticalTop(menuTop, menuHeight)
        if (barTop < menuTop + menuHeight && menuTop < barTop + barHeight) {
            menuTop = barTop + barNeed
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
