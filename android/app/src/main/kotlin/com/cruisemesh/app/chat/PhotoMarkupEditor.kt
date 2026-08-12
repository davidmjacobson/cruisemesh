package com.cruisemesh.app.chat

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Paint
import android.widget.Toast
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.drag
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.cruisemesh.app.R
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.MediaCompressor
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.roundToInt

/**
 * Full-screen "Draw" surface for a photo that is staged but not yet sent
 * (`specs/photo-markup.md`). A marker, not an art tool: freehand pen, four
 * colors, three widths, undo, clear.
 *
 * [jpeg] is the already-compressed staged blob, so its orientation is baked
 * into the pixels; nothing here re-reads or re-applies EXIF. [onConfirm] is
 * called with bytes that have already passed the staging size guard, so the
 * caller can stage them exactly as it stages a freshly picked photo.
 * Confirming without drawing anything hands [jpeg] straight back untouched.
 */
@Composable
fun PhotoMarkupEditor(
    jpeg: ByteArray,
    onCancel: () -> Unit,
    onConfirm: (ByteArray) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var source by remember(jpeg) { mutableStateOf<Bitmap?>(null) }
    var image by remember(jpeg) { mutableStateOf<ImageBitmap?>(null) }
    var decodeFailed by remember(jpeg) { mutableStateOf(false) }
    var drawing by remember(jpeg) { mutableStateOf(MarkupDrawing()) }
    var color by remember(jpeg) { mutableStateOf(MarkupColor.DEFAULT) }
    var thickness by remember(jpeg) { mutableStateOf(MarkupThickness.DEFAULT) }
    var viewport by remember(jpeg) { mutableStateOf(IntSize.Zero) }
    var working by remember(jpeg) { mutableStateOf(false) }

    LaunchedEffect(jpeg) {
        val decoded = withContext(Dispatchers.IO) {
            BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
        }
        source = decoded
        image = decoded?.asImageBitmap()
        decodeFailed = decoded == null
    }

    val currentImage = image
    val fit = remember(currentImage, viewport) {
        if (currentImage == null) {
            MarkupFit.NONE
        } else {
            markupFit(currentImage.width, currentImage.height, viewport.width, viewport.height)
        }
    }
    val longestEdge = currentImage?.let { maxOf(it.width, it.height) } ?: 1

    fun confirm() {
        val bitmap = source
        if (working) return
        if (markupConfirmPlan(drawing) == MarkupConfirmPlan.KEEP_ORIGINAL || bitmap == null) {
            onConfirm(jpeg)
            return
        }
        working = true
        scope.launch {
            val annotated = withContext(Dispatchers.IO) {
                compositeMarkup(bitmap, drawing.finish().strokes, longestEdge)
            }
            working = false
            // Strokes add detail and detail costs bytes, so a photo that fit
            // before annotation can stop fitting after it. Same warning an
            // oversized photo gets today, and the editor stays open so the
            // strokes can be undone rather than lost.
            if (markupSizeVerdict(annotated?.size, AttachmentPayload.MAX_BLOB_BYTES) ==
                MarkupSizeVerdict.FITS && annotated != null
            ) {
                onConfirm(annotated)
            } else {
                Toast.makeText(
                    context,
                    context.getString(R.string.ui_could_not_prepare_photo),
                    Toast.LENGTH_SHORT,
                ).show()
            }
        }
    }

    Dialog(
        onDismissRequest = onCancel,
        properties = DialogProperties(
            dismissOnBackPress = true,
            dismissOnClickOutside = false,
            usePlatformDefaultWidth = false,
            decorFitsSystemWindows = false,
        ),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black),
        ) {
            Column(modifier = Modifier.fillMaxSize()) {
                MarkupTopBar(
                    canUndo = drawing.canUndo,
                    enabled = !working,
                    onCancel = onCancel,
                    onUndo = { drawing = drawing.undo() },
                    onClear = { drawing = drawing.clear() },
                    onDone = { confirm() },
                )

                Box(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth(),
                    contentAlignment = Alignment.Center,
                ) {
                    if (decodeFailed) {
                        Text(
                            text = stringResource(R.string.ui_photo_could_not_be_displayed),
                            color = Color.White,
                            modifier = Modifier.padding(24.dp),
                        )
                    } else if (currentImage != null) {
                        MarkupCanvas(
                            image = currentImage,
                            fit = fit,
                            drawing = drawing,
                            longestEdge = longestEdge,
                            enabled = !working,
                            onMeasured = { viewport = it },
                            onBegin = { drawing = drawing.begin(color, thickness, fit.toImagePoint(it.asMarkupPoint())) },
                            onExtend = { drawing = drawing.extend(fit.toImagePoint(it.asMarkupPoint())) },
                            onFinish = { drawing = drawing.finish() },
                        )
                    }
                }

                MarkupToolBar(
                    color = color,
                    onColorChange = { color = it },
                    thickness = thickness,
                    onThicknessChange = { thickness = it },
                )
            }
        }
    }
}

@Composable
private fun MarkupTopBar(
    canUndo: Boolean,
    enabled: Boolean,
    onCancel: () -> Unit,
    onUndo: () -> Unit,
    onClear: () -> Unit,
    onDone: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .statusBarsPadding()
            .padding(horizontal = 4.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        TextButton(onClick = onCancel, enabled = enabled) {
            Text(stringResource(R.string.ui_cancel), color = Color.White)
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = onUndo, enabled = enabled && canUndo) {
                Text(stringResource(R.string.ui_undo), color = Color.White)
            }
            TextButton(onClick = onClear, enabled = enabled && canUndo) {
                Text(stringResource(R.string.ui_clear), color = Color.White)
            }
        }
        TextButton(onClick = onDone, enabled = enabled) {
            Text(stringResource(R.string.ui_done), color = Color.White)
        }
    }
}

/**
 * Draws the photo letterboxed inside the frame and the strokes on top of it,
 * both through the same [MarkupFit], so what the finger touches and what the
 * eye sees can never drift apart.
 */
@Composable
private fun MarkupCanvas(
    image: ImageBitmap,
    fit: MarkupFit,
    drawing: MarkupDrawing,
    longestEdge: Int,
    enabled: Boolean,
    onMeasured: (IntSize) -> Unit,
    onBegin: (Offset) -> Unit,
    onExtend: (Offset) -> Unit,
    onFinish: () -> Unit,
) {
    val drawingAreaLabel = stringResource(R.string.ui_drawing_area)
    // The gesture detectors are launched once and outlive recomposition, so
    // they must call through to the *current* callbacks -- otherwise a stroke
    // started after the frame was measured would still be mapped through the
    // fit that was in force when the detector started.
    val begin by rememberUpdatedState(onBegin)
    val extend by rememberUpdatedState(onExtend)
    val finish by rememberUpdatedState(onFinish)
    val drawingEnabled by rememberUpdatedState(enabled)
    Canvas(
        modifier = Modifier
            .fillMaxSize()
            .onSizeChanged(onMeasured)
            .pointerInput(Unit) {
                // A raw pointer loop rather than detectDragGestures: that
                // helper only reports a drag once touch slop is crossed, and
                // starts the stroke at the slop crossing rather than where the
                // finger landed -- so a circle came out as an open C and a mark
                // shorter than slop was never a drag at all. A pen has to start
                // exactly where it was put down. A press with no movement still
                // produces a one-point stroke, which paints as a dot.
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    // Once Done is pressed the bytes are being composited, so
                    // a stroke landing now would either be raced into the
                    // export or thrown away without the user knowing which.
                    // Match iOS and stop accepting ink.
                    if (!drawingEnabled) return@awaitEachGesture
                    begin(down.position)
                    try {
                        drag(down.id) { change ->
                            extend(change.position)
                            change.consume()
                        }
                    } finally {
                        // Reached on a lifted finger and on a cancelled gesture
                        // alike; a stroke is never left hanging under a finger
                        // that is gone.
                        finish()
                    }
                }
            }
            .semantics { contentDescription = drawingAreaLabel },
    ) {
        val drawWidth = (image.width * fit.scale).roundToInt()
        val drawHeight = (image.height * fit.scale).roundToInt()
        if (drawWidth <= 0 || drawHeight <= 0) return@Canvas
        drawImage(
            image = image,
            srcOffset = IntOffset.Zero,
            srcSize = IntSize(image.width, image.height),
            dstOffset = IntOffset(fit.offsetX.roundToInt(), fit.offsetY.roundToInt()),
            dstSize = IntSize(drawWidth, drawHeight),
        )
        // Strokes are clipped to the photo: a finger that slides off the edge
        // of a letterboxed image must not paint on the black surround.
        clipRect(
            left = fit.offsetX,
            top = fit.offsetY,
            right = fit.offsetX + drawWidth,
            bottom = fit.offsetY + drawHeight,
        ) {
            for (stroke in drawing.visibleStrokes) {
                val widthPx = stroke.thickness.widthPx(longestEdge) * fit.scale
                val paintColor = Color(stroke.color.argb)
                val view = stroke.points.map { fit.toViewPoint(it) }
                if (view.size == 1) {
                    drawCircle(
                        color = paintColor,
                        radius = widthPx / 2f,
                        center = Offset(view[0].x, view[0].y),
                    )
                    continue
                }
                val path = Path()
                view.forEachIndexed { index, point ->
                    if (index == 0) path.moveTo(point.x, point.y) else path.lineTo(point.x, point.y)
                }
                drawPath(
                    path = path,
                    color = paintColor,
                    style = Stroke(width = widthPx, cap = StrokeCap.Round, join = StrokeJoin.Round),
                )
            }
        }
    }
}

@Composable
private fun MarkupToolBar(
    color: MarkupColor,
    onColorChange: (MarkupColor) -> Unit,
    thickness: MarkupThickness,
    onThicknessChange: (MarkupThickness) -> Unit,
) {
    // Two rows rather than one: seven 48dp targets do not fit across a narrow
    // phone, and shrinking them below Android's minimum is not an option for a
    // control used with a thumb on a moving ship.
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .navigationBarsPadding()
            .padding(horizontal = 8.dp, vertical = 4.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.Center,
        ) {
            for (swatch in MarkupColor.entries) {
                val label = stringResource(markupColorLabel(swatch))
                Box(
                    // The hit target stays at Android's 48dp minimum; the
                    // swatch inside it is the visual size.
                    modifier = Modifier
                        .size(48.dp)
                        .selectable(selected = swatch == color, onClick = { onColorChange(swatch) })
                        .semantics { contentDescription = label },
                    contentAlignment = Alignment.Center,
                ) {
                    Box(
                        modifier = Modifier
                            .size(if (swatch == color) 32.dp else 26.dp)
                            .clip(CircleShape)
                            .background(Color(swatch.argb))
                            .border(
                                width = if (swatch == color) 3.dp else 1.dp,
                                color = if (swatch == color) {
                                    Color.White
                                } else {
                                    Color.White.copy(alpha = 0.35f)
                                },
                                shape = CircleShape,
                            ),
                    )
                }
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.Center,
        ) {
            for (step in MarkupThickness.entries) {
                val label = stringResource(markupThicknessLabel(step))
                val dot = when (step) {
                    MarkupThickness.THIN -> 8.dp
                    MarkupThickness.MEDIUM -> 14.dp
                    MarkupThickness.THICK -> 20.dp
                }
                Box(
                    modifier = Modifier
                        .size(48.dp)
                        .selectable(selected = step == thickness, onClick = { onThicknessChange(step) })
                        .semantics { contentDescription = label },
                    contentAlignment = Alignment.Center,
                ) {
                    Box(
                        modifier = Modifier
                            .size(dot)
                            .clip(CircleShape)
                            .background(
                                if (step == thickness) Color.White else Color.White.copy(alpha = 0.4f),
                            ),
                    )
                }
            }
        }
    }
}

/** The one place Compose's touch geometry crosses into the framework-free model. */
private fun Offset.asMarkupPoint(): MarkupPoint = MarkupPoint(x, y)

private fun markupColorLabel(color: MarkupColor): Int = when (color) {
    MarkupColor.RED -> R.string.ui_color_red
    MarkupColor.YELLOW -> R.string.ui_color_yellow
    MarkupColor.WHITE -> R.string.ui_color_white
    MarkupColor.BLACK -> R.string.ui_color_black
}

private fun markupThicknessLabel(thickness: MarkupThickness): Int = when (thickness) {
    MarkupThickness.THIN -> R.string.ui_pen_thin
    MarkupThickness.MEDIUM -> R.string.ui_pen_medium
    MarkupThickness.THICK -> R.string.ui_pen_thick
}

/**
 * Paints [strokes] onto a copy of [source] at the decoded photo's own
 * resolution and re-encodes through the staging compression path. Stroke
 * points are already in image coordinates, so nothing here depends on how big
 * the phone screen was. Returns null if the result cannot be made to fit,
 * which the caller turns into the oversized-photo warning.
 */
private fun compositeMarkup(source: Bitmap, strokes: List<MarkupStroke>, longestEdge: Int): ByteArray? {
    val canvasBitmap = try {
        source.copy(Bitmap.Config.ARGB_8888, true)
    } catch (_: OutOfMemoryError) {
        null
    } ?: return null
    return try {
        val canvas = android.graphics.Canvas(canvasBitmap)
        val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
        }
        for (stroke in strokes) {
            val width = stroke.thickness.widthPx(longestEdge)
            paint.color = stroke.color.argb
            paint.strokeWidth = width
            if (stroke.points.size == 1) {
                val point = stroke.points[0]
                paint.style = Paint.Style.FILL
                canvas.drawCircle(point.x, point.y, width / 2f, paint)
                paint.style = Paint.Style.STROKE
                continue
            }
            val path = android.graphics.Path()
            stroke.points.forEachIndexed { index, point ->
                if (index == 0) path.moveTo(point.x, point.y) else path.lineTo(point.x, point.y)
            }
            canvas.drawPath(path, paint)
        }
        MediaCompressor.compressBitmap(canvasBitmap)
    } finally {
        canvasBitmap.recycle()
    }
}
