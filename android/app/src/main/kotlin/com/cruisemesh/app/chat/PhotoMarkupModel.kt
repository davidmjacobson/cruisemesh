package com.cruisemesh.app.chat

/**
 * Drawing model and geometry for the pre-send photo markup editor
 * (`specs/photo-markup.md`).
 *
 * Deliberately free of Android and Compose imports so every rule here -- what
 * undo removes, how a finger position on screen becomes a pixel in the photo,
 * how wide a "thick" stroke is, whether an annotated photo still fits -- is a
 * plain unit test rather than an on-device experiment. The composable in
 * [PhotoMarkupEditor] holds one of these and does nothing but render it.
 */

/**
 * A point in whichever space the caller is working in: view pixels while a
 * finger is down, image pixels once [MarkupFit.toImagePoint] has mapped it.
 */
data class MarkupPoint(val x: Float, val y: Float)

/**
 * The four pen colors, as opaque ARGB. Picked to stay readable on both a
 * washed-out sunlit deck photo and a dark interior one. Red is first because
 * circling a spot is what people actually reach for this to do.
 */
enum class MarkupColor(val argb: Int) {
    RED(0xFFE53935.toInt()),
    YELLOW(0xFFFFD400.toInt()),
    WHITE(0xFFFFFFFF.toInt()),
    BLACK(0xFF111111.toInt()),
    ;

    companion object {
        val DEFAULT = RED
    }
}

/**
 * Pen width as a fraction of the photo's longest edge, so the same step looks
 * the same on a wide deck plan and a tall screenshot, and so a stroke drawn on
 * a small phone lands at the right weight in the full-resolution composite.
 */
enum class MarkupThickness(val edgeFraction: Float) {
    THIN(0.006f),
    MEDIUM(0.013f),
    THICK(0.026f),
    ;

    /** Stroke width in pixels for an image whose longest edge is [longestEdgePx]. */
    fun widthPx(longestEdgePx: Int): Float =
        (edgeFraction * longestEdgePx.coerceAtLeast(1)).coerceAtLeast(1f)

    companion object {
        val DEFAULT = MEDIUM
    }
}

/** One freehand stroke: a color, a width step, and the path the finger took. */
data class MarkupStroke(
    val color: MarkupColor,
    val thickness: MarkupThickness,
    val points: List<MarkupPoint>,
)

/**
 * How a photo of [imageWidth] x [imageHeight] sits inside a viewport, letterboxed
 * and centered (the editor shows the whole photo, never a crop). [scale] is
 * image pixels -> view pixels; [offsetX]/[offsetY] are the letterbox margins.
 */
data class MarkupFit(
    val scale: Float,
    val offsetX: Float,
    val offsetY: Float,
) {
    /** Maps a touch in view coordinates back to a pixel in the decoded image. */
    fun toImagePoint(view: MarkupPoint): MarkupPoint =
        MarkupPoint((view.x - offsetX) / scale, (view.y - offsetY) / scale)

    /** Maps an image pixel forward to where it is drawn on screen. */
    fun toViewPoint(image: MarkupPoint): MarkupPoint =
        MarkupPoint(image.x * scale + offsetX, image.y * scale + offsetY)

    companion object {
        /** Identity fit, used before the editor has been measured. */
        val NONE = MarkupFit(scale = 1f, offsetX = 0f, offsetY = 0f)
    }
}

/**
 * Computes the letterboxed fit of an image inside a viewport. Returns
 * [MarkupFit.NONE] when anything is degenerate, so an unmeasured frame maps
 * points through unchanged instead of dividing by zero.
 */
fun markupFit(imageWidth: Int, imageHeight: Int, viewWidth: Int, viewHeight: Int): MarkupFit {
    if (imageWidth <= 0 || imageHeight <= 0 || viewWidth <= 0 || viewHeight <= 0) return MarkupFit.NONE
    val scale = minOf(
        viewWidth.toFloat() / imageWidth.toFloat(),
        viewHeight.toFloat() / imageHeight.toFloat(),
    )
    return MarkupFit(
        scale = scale,
        offsetX = (viewWidth - imageWidth * scale) / 2f,
        offsetY = (viewHeight - imageHeight * scale) / 2f,
    )
}

/**
 * Every stroke drawn so far, plus the one still under the finger. Immutable:
 * each edit returns a new drawing, which is what makes undo exact -- there is
 * no partially-mutated canvas to recover from.
 *
 * Points are stored in **image** coordinates. Compositing therefore happens at
 * the decoded photo's resolution, and drawing on a small screen never shrinks
 * the photo that gets sent.
 */
data class MarkupDrawing(
    val strokes: List<MarkupStroke> = emptyList(),
    val active: MarkupStroke? = null,
) {
    /** Committed strokes plus the in-progress one, in paint order. */
    val visibleStrokes: List<MarkupStroke>
        get() = if (active == null) strokes else strokes + active

    /** True once there is anything at all to composite. */
    val hasStrokes: Boolean
        get() = strokes.isNotEmpty() || active != null

    /** Whether the Undo control has anything left to take back. */
    val canUndo: Boolean
        get() = hasStrokes

    fun begin(color: MarkupColor, thickness: MarkupThickness, at: MarkupPoint): MarkupDrawing =
        copy(active = MarkupStroke(color, thickness, listOf(at)))

    /** Extends the in-progress stroke. A no-op if no stroke is in progress. */
    fun extend(to: MarkupPoint): MarkupDrawing {
        val current = active ?: return this
        return copy(active = current.copy(points = current.points + to))
    }

    /** Commits the in-progress stroke. A single tap still counts -- it's a dot. */
    fun finish(): MarkupDrawing {
        val current = active ?: return this
        return MarkupDrawing(strokes = strokes + current, active = null)
    }

    /**
     * Removes exactly one complete stroke -- the most recent one. An
     * in-progress stroke is dropped first, so undo can never leave half a
     * scribble behind. Repeatable back to a clean image; there is no redo.
     */
    fun undo(): MarkupDrawing = when {
        active != null -> copy(active = null)
        strokes.isEmpty() -> this
        else -> MarkupDrawing(strokes = strokes.dropLast(1), active = null)
    }

    /** Empties the canvas. No prompt: cancelling out of the editor undoes it. */
    fun clear(): MarkupDrawing = MarkupDrawing()
}

/**
 * What confirming should actually do.
 *
 * With nothing drawn the staged bytes are handed straight back: no decode, no
 * re-encode, no orientation pass. That is what keeps "open the editor, change
 * nothing, confirm" byte-identical -- and keeps an upright photo upright,
 * since it never touches the pixels at all.
 */
enum class MarkupConfirmPlan {
    KEEP_ORIGINAL,
    COMPOSITE_AND_REENCODE,
}

fun markupConfirmPlan(drawing: MarkupDrawing): MarkupConfirmPlan =
    if (drawing.hasStrokes) MarkupConfirmPlan.COMPOSITE_AND_REENCODE else MarkupConfirmPlan.KEEP_ORIGINAL

/**
 * The staging size guard, re-run on the annotated result. Strokes add detail,
 * detail costs bytes, and a photo that fit before annotation can stop fitting
 * after it. [TOO_LARGE] means the caller must show the same plain-speech
 * warning an oversized photo gets today -- never a silent drop.
 */
enum class MarkupSizeVerdict {
    FITS,
    TOO_LARGE,
}

/**
 * [encodedBytes] is the size of the re-encoded JPEG, or null when compression
 * gave up entirely. [maxBlobBytes] is the attachment ceiling
 * (`AttachmentPayload.MAX_BLOB_BYTES`).
 */
fun markupSizeVerdict(encodedBytes: Int?, maxBlobBytes: Int): MarkupSizeVerdict =
    if (encodedBytes == null || encodedBytes <= 0 || encodedBytes > maxBlobBytes) {
        MarkupSizeVerdict.TOO_LARGE
    } else {
        MarkupSizeVerdict.FITS
    }
