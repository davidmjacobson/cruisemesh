package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Rules of the pre-send photo markup editor (specs/photo-markup.md). */
class PhotoMarkupModelTest {

    @Test
    fun `a wide photo letterboxes top and bottom and maps touches back to pixels`() {
        // 1000x500 image in a 500x500 frame: fits on width, 125px bars above
        // and below.
        val fit = markupFit(imageWidth = 1000, imageHeight = 500, viewWidth = 500, viewHeight = 500)

        assertEquals(0.5f, fit.scale, 0.0001f)
        assertEquals(0f, fit.offsetX, 0.0001f)
        assertEquals(125f, fit.offsetY, 0.0001f)

        // A finger in the middle of the frame is the middle of the photo.
        val center = fit.toImagePoint(MarkupPoint(250f, 250f))
        assertEquals(500f, center.x, 0.0001f)
        assertEquals(250f, center.y, 0.0001f)

        // Top-left of the drawn photo is pixel (0, 0), not the frame corner.
        val corner = fit.toImagePoint(MarkupPoint(0f, 125f))
        assertEquals(0f, corner.x, 0.0001f)
        assertEquals(0f, corner.y, 0.0001f)
    }

    @Test
    fun `a tall photo letterboxes left and right`() {
        val fit = markupFit(imageWidth = 400, imageHeight = 800, viewWidth = 400, viewHeight = 400)

        assertEquals(0.5f, fit.scale, 0.0001f)
        assertEquals(100f, fit.offsetX, 0.0001f)
        assertEquals(0f, fit.offsetY, 0.0001f)
        assertEquals(600f, fit.toImagePoint(MarkupPoint(200f, 300f)).y, 0.0001f)
    }

    @Test
    fun `view to image and back is a round trip`() {
        val fit = markupFit(imageWidth = 1024, imageHeight = 768, viewWidth = 360, viewHeight = 640)
        val touch = MarkupPoint(123.5f, 301.25f)

        val roundTripped = fit.toViewPoint(fit.toImagePoint(touch))

        assertEquals(touch.x, roundTripped.x, 0.001f)
        assertEquals(touch.y, roundTripped.y, 0.001f)
    }

    @Test
    fun `an unmeasured frame maps points through unchanged instead of dividing by zero`() {
        val fit = markupFit(imageWidth = 1024, imageHeight = 768, viewWidth = 0, viewHeight = 0)

        assertEquals(MarkupFit.NONE, fit)
        assertEquals(MarkupPoint(7f, 9f), fit.toImagePoint(MarkupPoint(7f, 9f)))
    }

    @Test
    fun `strokes are stored in image pixels so a small screen does not shrink the photo`() {
        // Drawing on a 360x640 phone over a 1024x768 photo.
        val fit = markupFit(imageWidth = 1024, imageHeight = 768, viewWidth = 360, viewHeight = 640)
        val drawing = MarkupDrawing()
            .begin(MarkupColor.RED, MarkupThickness.MEDIUM, fit.toImagePoint(MarkupPoint(0f, fit.offsetY)))
            .extend(fit.toImagePoint(MarkupPoint(360f, fit.offsetY + 768 * fit.scale)))
            .finish()

        val points = drawing.strokes.single().points
        assertEquals(0f, points.first().x, 0.01f)
        assertEquals(0f, points.first().y, 0.01f)
        assertEquals(1024f, points.last().x, 0.01f)
        assertEquals(768f, points.last().y, 0.01f)
    }

    @Test
    fun `undo removes exactly one complete stroke`() {
        val drawing = strokeAt(MarkupDrawing(), 1f)
            .let { strokeAt(it, 2f) }
            .let { strokeAt(it, 3f) }
        assertEquals(3, drawing.strokes.size)

        val once = drawing.undo()
        assertEquals(2, once.strokes.size)
        assertEquals(listOf(1f, 2f), once.strokes.map { it.points.first().x })

        val twice = once.undo()
        assertEquals(1, twice.strokes.size)
        assertEquals(listOf(1f), twice.strokes.map { it.points.first().x })
    }

    @Test
    fun `undo is repeatable back to a clean image and then does nothing`() {
        var drawing = strokeAt(strokeAt(MarkupDrawing(), 1f), 2f)

        repeat(4) { drawing = drawing.undo() }

        assertTrue(drawing.strokes.isEmpty())
        assertNull(drawing.active)
        assertFalse(drawing.hasStrokes)
        assertFalse(drawing.canUndo)
    }

    @Test
    fun `undo drops an in-progress stroke first so half a scribble is never left behind`() {
        val drawing = strokeAt(MarkupDrawing(), 1f)
            .begin(MarkupColor.RED, MarkupThickness.MEDIUM, MarkupPoint(9f, 9f))
            .extend(MarkupPoint(10f, 10f))

        val undone = drawing.undo()

        assertNull(undone.active)
        assertEquals(1, undone.strokes.size)
    }

    @Test
    fun `clear empties the canvas including a stroke still under the finger`() {
        val drawing = strokeAt(strokeAt(MarkupDrawing(), 1f), 2f)
            .begin(MarkupColor.WHITE, MarkupThickness.THICK, MarkupPoint(5f, 5f))

        val cleared = drawing.clear()

        assertTrue(cleared.strokes.isEmpty())
        assertNull(cleared.active)
        assertFalse(cleared.hasStrokes)
        assertTrue(cleared.visibleStrokes.isEmpty())
    }

    @Test
    fun `an in-progress stroke is visible before it is committed`() {
        val drawing = MarkupDrawing()
            .begin(MarkupColor.YELLOW, MarkupThickness.THIN, MarkupPoint(1f, 1f))
            .extend(MarkupPoint(2f, 2f))

        assertTrue(drawing.strokes.isEmpty())
        assertEquals(1, drawing.visibleStrokes.size)
        assertEquals(2, drawing.visibleStrokes.single().points.size)

        val finished = drawing.finish()
        assertEquals(1, finished.strokes.size)
        assertNull(finished.active)
    }

    @Test
    fun `a single tap still counts as a stroke`() {
        val drawing = MarkupDrawing()
            .begin(MarkupColor.RED, MarkupThickness.MEDIUM, MarkupPoint(4f, 4f))
            .finish()

        assertEquals(1, drawing.strokes.size)
        assertEquals(1, drawing.strokes.single().points.size)
    }

    @Test
    fun `confirming with nothing drawn keeps the original bytes untouched`() {
        assertEquals(MarkupConfirmPlan.KEEP_ORIGINAL, markupConfirmPlan(MarkupDrawing()))
        assertEquals(
            MarkupConfirmPlan.KEEP_ORIGINAL,
            markupConfirmPlan(strokeAt(MarkupDrawing(), 1f).undo()),
        )
        assertEquals(
            MarkupConfirmPlan.KEEP_ORIGINAL,
            markupConfirmPlan(strokeAt(MarkupDrawing(), 1f).clear()),
        )
    }

    @Test
    fun `confirming after drawing composites and re-encodes`() {
        assertEquals(
            MarkupConfirmPlan.COMPOSITE_AND_REENCODE,
            markupConfirmPlan(strokeAt(MarkupDrawing(), 1f)),
        )
        assertEquals(
            MarkupConfirmPlan.COMPOSITE_AND_REENCODE,
            markupConfirmPlan(MarkupDrawing().begin(MarkupColor.RED, MarkupThickness.THIN, MarkupPoint(0f, 0f))),
        )
    }

    @Test
    fun `the size guard rejects an annotated photo that no longer fits`() {
        val max = 180 * 1024

        assertEquals(MarkupSizeVerdict.FITS, markupSizeVerdict(48 * 1024, max))
        assertEquals(MarkupSizeVerdict.FITS, markupSizeVerdict(max, max))
        assertEquals(MarkupSizeVerdict.TOO_LARGE, markupSizeVerdict(max + 1, max))
        // Compression gave up entirely.
        assertEquals(MarkupSizeVerdict.TOO_LARGE, markupSizeVerdict(null, max))
        // An empty encode is a failure, not a very small photo.
        assertEquals(MarkupSizeVerdict.TOO_LARGE, markupSizeVerdict(0, max))
    }

    @Test
    fun `pen width scales with the photo so a thick stroke looks the same on any image`() {
        val small = MarkupThickness.THICK.widthPx(512)
        val large = MarkupThickness.THICK.widthPx(1024)

        assertEquals(2f, large / small, 0.0001f)
        assertTrue(MarkupThickness.THIN.widthPx(1024) < MarkupThickness.MEDIUM.widthPx(1024))
        assertTrue(MarkupThickness.MEDIUM.widthPx(1024) < MarkupThickness.THICK.widthPx(1024))
        // Never a zero-width pen, however tiny the image.
        assertTrue(MarkupThickness.THIN.widthPx(1) >= 1f)
        assertTrue(MarkupThickness.THIN.widthPx(0) >= 1f)
    }

    @Test
    fun `red is the default pen and medium the default width`() {
        assertEquals(MarkupColor.RED, MarkupColor.DEFAULT)
        assertEquals(MarkupThickness.MEDIUM, MarkupThickness.DEFAULT)
    }

    @Test
    fun `every pen color is fully opaque and distinct`() {
        val colors = MarkupColor.entries
        assertEquals(4, colors.size)
        assertEquals(colors.size, colors.map { it.argb }.toSet().size)
        for (color in colors) {
            assertNotEquals(0, color.argb ushr 24)
            assertEquals(0xFF, color.argb ushr 24)
        }
    }

    private fun strokeAt(drawing: MarkupDrawing, x: Float): MarkupDrawing =
        drawing.begin(MarkupColor.RED, MarkupThickness.MEDIUM, MarkupPoint(x, x))
            .extend(MarkupPoint(x + 1f, x + 1f))
            .finish()
}
