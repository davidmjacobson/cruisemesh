package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

class OverlayPlacementTest {

    // Shared viewport/element sizes for the vertical-rule tests; only bubbleBounds varies.
    private val screenTop = 0f
    private val screenBottom = 800f
    private val screenLeft = 0f
    private val screenRight = 400f
    private val spacing = 8f
    private val margin = 16f
    private val barWidth = 200f
    private val barHeight = 50f
    private val menuWidth = 150f
    private val menuHeight = 100f

    private fun compute(
        bounds: OverlayPlacement.Bounds,
        isOwn: Boolean = false,
        screenBottom: Float = this.screenBottom,
    ) = OverlayPlacement.compute(
        bubbleBounds = bounds,
        barWidth = barWidth,
        barHeight = barHeight,
        menuWidth = menuWidth,
        menuHeight = menuHeight,
        screenTop = screenTop,
        screenBottom = screenBottom,
        screenLeft = screenLeft,
        screenRight = screenRight,
        spacing = spacing,
        margin = margin,
        isOwn = isOwn,
    )

    @Test
    fun `bubble comfortably mid-screen needs no shift`() {
        val result = compute(OverlayPlacement.Bounds(left = 50f, top = 300f, right = 250f, bottom = 350f))
        assertEquals(300f, result.bubbleTop)
        assertEquals(242f, result.barTop)
        assertEquals(358f, result.menuTop)
    }

    @Test
    fun `bar exactly touching the top edge needs no shift`() {
        val result = compute(OverlayPlacement.Bounds(left = 50f, top = 58f, right = 250f, bottom = 108f))
        assertEquals(58f, result.bubbleTop)
        assertEquals(0f, result.barTop)
        assertEquals(116f, result.menuTop)
    }

    @Test
    fun `bar one pixel past the top edge keeps bubble anchored and stacks controls below`() {
        val result = compute(OverlayPlacement.Bounds(left = 50f, top = 57f, right = 250f, bottom = 107f))
        assertEquals(57f, result.bubbleTop)
        assertEquals(115f, result.barTop)
        assertEquals(173f, result.menuTop)
    }

    @Test
    fun `menu exactly touching the bottom edge needs no shift`() {
        val result = compute(OverlayPlacement.Bounds(left = 50f, top = 642f, right = 250f, bottom = 692f))
        assertEquals(642f, result.bubbleTop)
        assertEquals(584f, result.barTop)
        assertEquals(700f, result.menuTop)
    }

    @Test
    fun `menu one pixel past the bottom edge keeps bubble anchored and stacks controls above`() {
        val result = compute(OverlayPlacement.Bounds(left = 50f, top = 643f, right = 250f, bottom = 693f))
        assertEquals(643f, result.bubbleTop)
        assertEquals(477f, result.barTop)
        assertEquals(535f, result.menuTop)
    }

    @Test
    fun `stack taller than the viewport keeps bubble anchor and clamps controls`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 50f, top = 1000f, right = 250f, bottom = 1050f),
            screenBottom = 200f,
        )
        assertEquals(150f, result.barTop)
        assertEquals(1000f, result.bubbleTop)
        assertEquals(100f, result.menuTop)
    }

    @Test
    fun `own message aligns bar and menu to the bubble's right edge`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 250f, top = 300f, right = 350f, bottom = 350f),
            isOwn = true,
        )
        assertEquals(150f, result.barLeft) // 350 - 200
        assertEquals(200f, result.menuLeft) // 350 - 150
    }

    @Test
    fun `other message aligns bar and menu to the bubble's left edge`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 100f, top = 300f, right = 200f, bottom = 350f),
            isOwn = false,
        )
        assertEquals(100f, result.barLeft)
        assertEquals(100f, result.menuLeft)
    }

    @Test
    fun `alignment clamps to the screen margin near the right edge`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 310f, top = 300f, right = 390f, bottom = 350f),
            isOwn = true,
        )
        // Unclamped would be 390-200=190 and 390-150=240; both must stay inside the 16px margin.
        assertEquals(184f, result.barLeft) // 400 - 16 - 200
        assertEquals(234f, result.menuLeft) // 400 - 16 - 150
    }

    @Test
    fun `alignment clamps to the screen margin near the left edge`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = -30f, top = 300f, right = 50f, bottom = 350f),
            isOwn = false,
        )
        assertEquals(16f, result.barLeft)
        assertEquals(16f, result.menuLeft)
    }
}
