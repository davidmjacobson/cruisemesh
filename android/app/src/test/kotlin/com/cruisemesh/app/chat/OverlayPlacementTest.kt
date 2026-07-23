package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
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
        screenTop: Float = this.screenTop,
        screenBottom: Float = this.screenBottom,
        barHeight: Float = this.barHeight,
        menuHeight: Float = this.menuHeight,
        spacing: Float = this.spacing,
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
    fun `bubble past the bottom of the viewport stacks controls above the clipped anchor`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 50f, top = 1000f, right = 250f, bottom = 1050f),
            screenBottom = 200f,
        )
        assertEquals(1000f, result.bubbleTop)
        // Anchor clips to the 200px bottom edge; menu hugs it, bar stacks above.
        assertEquals(92f, result.menuTop)
        assertEquals(34f, result.barTop)
        assertNoOverlap(result)
    }

    // Regression: Pixel 7 field repro (2026-07-23), device px. A 21-line bubble
    // left ~435px above and ~255px below -- the menu fit in neither natural
    // slot, and the old order (resolve overlap, then clamp) clamped the bar
    // from y=45 back down onto the menu at y=223. The split arrangement keeps
    // the menu above and moves the bar below instead.
    @Test
    fun `tall bubble with a cramped top splits menu above and bar below`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 252f, top = 622f, right = 1037f, bottom = 2040f),
            screenTop = 187f,
            screenBottom = 2295f,
            barHeight = 157f,
            menuHeight = 396f,
            spacing = 21f,
        )
        assertEquals(205f, result.menuTop) // 622 - 21 - 396, above the bubble
        assertEquals(2061f, result.barTop) // 2040 + 21, below the bubble
        assertNoOverlap(result, barHeight = 157f, menuHeight = 396f)
    }

    @Test
    fun `bubble filling the whole viewport pins bar top and menu bottom without collision`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 50f, top = -100f, right = 250f, bottom = 900f),
        )
        assertEquals(0f, result.barTop)
        assertEquals(700f, result.menuTop) // 800 - 100
        assertNoOverlap(result)
    }

    @Test
    fun `bubble scrolled past the top keeps controls inside the viewport`() {
        val result = compute(
            bounds = OverlayPlacement.Bounds(left = 50f, top = -150f, right = 250f, bottom = 60f),
        )
        // No room above the clipped anchor: bar and menu stack below it.
        assertEquals(68f, result.barTop)
        assertEquals(126f, result.menuTop)
        assertNoOverlap(result)
    }

    // Sweep the whole placement family: for every bubble height/position
    // combination the bar and menu must stay inside the viewport and must
    // never overlap each other, no matter which arrangement was picked.
    @Test
    fun `no bubble geometry ever overlaps bar and menu or leaves the viewport`() {
        val heights = listOf(30f, 200f, 500f, 750f, 1200f)
        for (height in heights) {
            var top = -1300f
            while (top <= 900f) {
                val result = compute(OverlayPlacement.Bounds(left = 50f, top = top, right = 250f, bottom = top + height))
                val label = "bubble top=$top height=$height"
                assertTrue("$label: bar off-screen (barTop=${result.barTop})", result.barTop >= screenTop)
                assertTrue("$label: bar off-screen (barTop=${result.barTop})", result.barTop + barHeight <= screenBottom)
                assertTrue("$label: menu off-screen (menuTop=${result.menuTop})", result.menuTop >= screenTop)
                assertTrue("$label: menu off-screen (menuTop=${result.menuTop})", result.menuTop + menuHeight <= screenBottom)
                assertNoOverlap(result, label = label)
                top += 25f
            }
        }
    }

    private fun assertNoOverlap(
        result: OverlayPlacement.Result,
        barHeight: Float = this.barHeight,
        menuHeight: Float = this.menuHeight,
        label: String = "",
    ) {
        val overlap = result.barTop < result.menuTop + menuHeight && result.menuTop < result.barTop + barHeight
        assertTrue(
            "$label bar [${result.barTop}, ${result.barTop + barHeight}] overlaps " +
                "menu [${result.menuTop}, ${result.menuTop + menuHeight}]",
            !overlap,
        )
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
