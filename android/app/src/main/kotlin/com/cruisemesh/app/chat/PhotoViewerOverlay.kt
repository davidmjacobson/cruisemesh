package com.cruisemesh.app.chat

import android.graphics.BitmapFactory
import android.widget.Toast
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.cruisemesh.app.media.ImageGallery

private const val MIN_SCALE = 1f
private const val MAX_SCALE = 5f
private const val DOUBLE_TAP_SCALE = 2.5f

internal fun clampedPhotoScale(current: Float, zoom: Float): Float =
    (current * zoom).coerceIn(MIN_SCALE, MAX_SCALE)

internal fun clampedPhotoOffset(offset: Offset, scale: Float, viewport: IntSize): Offset {
    if (scale <= MIN_SCALE || viewport.width <= 0 || viewport.height <= 0) return Offset.Zero
    val maxX = viewport.width * (scale - MIN_SCALE) / 2f
    val maxY = viewport.height * (scale - MIN_SCALE) / 2f
    return Offset(
        x = offset.x.coerceIn(-maxX, maxX),
        y = offset.y.coerceIn(-maxY, maxY),
    )
}

internal fun shouldDismissPhotoViewer(scale: Float, verticalDrag: Float, threshold: Float): Boolean =
    scale <= MIN_SCALE && verticalDrag >= threshold

/** Full-screen photo surface with zoom, pan, double-tap, swipe-down, back, and save. */
@Composable
fun PhotoViewerOverlay(
    jpeg: ByteArray,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val bitmap = remember(jpeg) {
        BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)?.asImageBitmap()
    }
    var scale by remember(jpeg) { mutableFloatStateOf(MIN_SCALE) }
    var offset by remember(jpeg) { mutableStateOf(Offset.Zero) }
    var viewport by remember { mutableStateOf(IntSize.Zero) }
    var dismissDragY by remember(jpeg) { mutableFloatStateOf(0f) }
    val dismissThreshold = with(LocalDensity.current) { 120.dp.toPx() }

    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(
            dismissOnBackPress = true,
            dismissOnClickOutside = false,
            usePlatformDefaultWidth = false,
            decorFitsSystemWindows = false,
        ),
    ) {
        val swipeDownModifier = if (scale <= MIN_SCALE) {
            Modifier.pointerInput(jpeg, dismissThreshold) {
                detectVerticalDragGestures(
                    onVerticalDrag = { change, dragAmount ->
                        if (dragAmount > 0f || dismissDragY > 0f) {
                            change.consume()
                            dismissDragY = (dismissDragY + dragAmount).coerceAtLeast(0f)
                        }
                    },
                    onDragEnd = {
                        if (shouldDismissPhotoViewer(scale, dismissDragY, dismissThreshold)) {
                            onDismiss()
                        } else {
                            dismissDragY = 0f
                        }
                    },
                    onDragCancel = { dismissDragY = 0f },
                )
            }
        } else {
            Modifier
        }

        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
                .onSizeChanged { viewport = it }
                .then(swipeDownModifier)
                .pointerInput(jpeg, viewport) {
                    detectTransformGestures { _, pan, zoom, _ ->
                        val nextScale = clampedPhotoScale(scale, zoom)
                        if (nextScale > MIN_SCALE) {
                            dismissDragY = 0f
                            scale = nextScale
                            offset = clampedPhotoOffset(offset + pan, scale, viewport)
                        } else {
                            scale = MIN_SCALE
                            offset = Offset.Zero
                        }
                    }
                }
                .pointerInput(jpeg) {
                    detectTapGestures(
                        onDoubleTap = {
                            if (scale > MIN_SCALE) {
                                scale = MIN_SCALE
                                offset = Offset.Zero
                            } else {
                                scale = DOUBLE_TAP_SCALE
                            }
                            dismissDragY = 0f
                        },
                    )
                }
                .semantics { contentDescription = "Full-screen photo viewer" },
        ) {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = "Full-screen photo",
                    contentScale = ContentScale.Fit,
                    modifier = Modifier
                        .fillMaxSize()
                        .graphicsLayer {
                            scaleX = scale
                            scaleY = scale
                            translationX = offset.x
                            translationY = if (scale <= MIN_SCALE) dismissDragY else offset.y
                        },
                )
            } else {
                Text(
                    text = "Photo could not be displayed",
                    color = Color.White,
                    modifier = Modifier.padding(24.dp),
                )
            }

            Row(
                horizontalArrangement = Arrangement.SpaceBetween,
                modifier = Modifier
                    .fillMaxWidth()
                    .statusBarsPadding()
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            ) {
                IconButton(onClick = onDismiss) {
                    Icon(Icons.Default.Close, contentDescription = "Close photo", tint = Color.White)
                }
                TextButton(
                    onClick = {
                        val saved = ImageGallery.saveJpeg(context, jpeg)
                        Toast.makeText(
                            context,
                            if (saved != null) "Saved to Pictures/CruiseMesh" else "Could not save image",
                            Toast.LENGTH_SHORT,
                        ).show()
                    },
                ) {
                    Text("Save", color = Color.White)
                }
            }
        }
    }
}
