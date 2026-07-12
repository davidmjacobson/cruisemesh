package com.cruisemesh.app.chat

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

/** Captured at long-press time: which message and where its bubble sits on screen (root coords). */
data class FocusedMessage(
    val target: MessageTarget,
    val bounds: Rect,
)

private val OVERLAY_SPACING = 8.dp
private val OVERLAY_MARGIN = 16.dp
/** Extra breathing room beyond the system status/navigation bars so the bar and menu never sit flush against a screen edge. */
private val OVERLAY_EDGE_CLEARANCE = 16.dp
private const val SCRIM_ALPHA = 0.55f
private const val ENTRANCE_MS = 150
private const val EXIT_MS = 120

// Rough pre-measurement estimate for the bar (6 circular 40dp reaction
// buttons) and menu (two DropdownMenuItems) so the very first frame already
// places them close to correct instead of snapping in from a zero-size guess.
private val ESTIMATED_BAR_SIZE = DpSize(268.dp, 52.dp)
private val ESTIMATED_MENU_SIZE = DpSize(180.dp, 96.dp)

/**
 * MESSAGE_LONGPRESS_OVERLAY.md: full-screen scrim over everything (list, top
 * bar, composer) with the pressed bubble re-drawn undimmed at its original
 * screen position via [bubbleContent], plus a floating reaction bar above it
 * and an action menu below -- as overlay layers, not inserted into the
 * message stream. [focused].bounds anchors the bubble; [OverlayPlacement]
 * works out where the bar/menu land.
 */
@Composable
fun MessageFocusOverlay(
    focused: FocusedMessage,
    isOwn: Boolean,
    canCopy: Boolean,
    ownReactionEmoji: String?,
    onDismiss: () -> Unit,
    onReact: (String) -> Unit,
    onCopy: () -> Unit,
    onInfo: () -> Unit,
    bubbleContent: @Composable () -> Unit,
) {
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    val entrance = remember { Animatable(0f) }
    val pulse = remember { Animatable(1f) }
    var dismissing by remember { mutableStateOf(false) }

    fun dismiss() {
        if (dismissing) return
        dismissing = true
        scope.launch {
            entrance.animateTo(0f, tween(EXIT_MS))
            onDismiss()
        }
    }

    LaunchedEffect(Unit) {
        launch { entrance.animateTo(1f, tween(ENTRANCE_MS)) }
        pulse.animateTo(1.05f, spring(dampingRatio = Spring.DampingRatioMediumBouncy))
        pulse.animateTo(1f, spring(dampingRatio = Spring.DampingRatioMediumBouncy))
    }

    BackHandler(onBack = ::dismiss)

    val estimatedBarSize = with(density) {
        IntSize(ESTIMATED_BAR_SIZE.width.roundToPx(), ESTIMATED_BAR_SIZE.height.roundToPx())
    }
    val estimatedMenuSize = with(density) {
        IntSize(ESTIMATED_MENU_SIZE.width.roundToPx(), ESTIMATED_MENU_SIZE.height.roundToPx())
    }
    var barSize by remember { mutableStateOf(estimatedBarSize) }
    var menuSize by remember { mutableStateOf(estimatedMenuSize) }

    BoxWithConstraints(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black.copy(alpha = SCRIM_ALPHA * entrance.value))
            .pointerInput(Unit) { detectTapGestures { dismiss() } }
            .semantics { contentDescription = "Dismiss message options" },
    ) {
        val edgeClearancePx = with(density) { OVERLAY_EDGE_CLEARANCE.toPx() }
        val screenRightPx = with(density) { maxWidth.toPx() }
        // Keep the bar/menu clear of the status bar and nav bar/gesture pill,
        // plus a little extra padding so they never sit flush on an edge.
        val screenTopPx = WindowInsets.statusBars.getTop(density).toFloat() + edgeClearancePx
        val screenBottomPx = with(density) { maxHeight.toPx() } -
            WindowInsets.navigationBars.getBottom(density).toFloat() - edgeClearancePx
        val spacingPx = with(density) { OVERLAY_SPACING.toPx() }
        val marginPx = with(density) { OVERLAY_MARGIN.toPx() }

        val placement = OverlayPlacement.compute(
            bubbleBounds = OverlayPlacement.Bounds(
                left = focused.bounds.left,
                top = focused.bounds.top,
                right = focused.bounds.right,
                bottom = focused.bounds.bottom,
            ),
            barWidth = barSize.width.toFloat(),
            barHeight = barSize.height.toFloat(),
            menuWidth = menuSize.width.toFloat(),
            menuHeight = menuSize.height.toFloat(),
            screenTop = screenTopPx,
            screenBottom = screenBottomPx,
            screenLeft = 0f,
            screenRight = screenRightPx,
            spacing = spacingPx,
            margin = marginPx,
            isOwn = isOwn,
        )

        // The focused bubble copy: undimmed (drawn above the scrim background),
        // pulses once on open, swallows taps so tapping it doesn't dismiss.
        Box(
            modifier = Modifier
                .offset(focused.bounds.left, placement.bubbleTop)
                .graphicsLayer {
                    scaleX = pulse.value
                    scaleY = pulse.value
                }
                .pointerInput(Unit) { detectTapGestures { } },
        ) {
            bubbleContent()
        }

        Box(
            modifier = Modifier
                .offset(placement.barLeft, placement.barTop)
                .onSizeChanged { barSize = it }
                .graphicsLayer { alpha = entrance.value }
                .pointerInput(Unit) { detectTapGestures { } },
        ) {
            ReactionPickerBar(
                selectedEmoji = ownReactionEmoji,
                onReact = { emoji ->
                    onReact(emoji)
                },
            )
        }

        Box(
            modifier = Modifier
                .offset(placement.menuLeft, placement.menuTop)
                .onSizeChanged { menuSize = it }
                .graphicsLayer { alpha = entrance.value }
                .pointerInput(Unit) { detectTapGestures { } },
        ) {
            MessageActionPanel(
                canCopy = canCopy,
                onCopy = onCopy,
                onInfo = onInfo,
            )
        }
    }
}

/** Positions a child at an absolute (px, px) offset within the enclosing Box -- root coordinates from [FocusedMessage.bounds] / [OverlayPlacement] land here directly. */
private fun Modifier.offset(xPx: Float, yPx: Float): Modifier =
    this.offset { IntOffset(xPx.toInt(), yPx.toInt()) }

@Composable
private fun ReactionPickerBar(
    selectedEmoji: String?,
    onReact: (String) -> Unit,
) {
    Surface(
        shape = RoundedCornerShape(28.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        tonalElevation = 6.dp,
        shadowElevation = 6.dp,
        modifier = Modifier.padding(bottom = 6.dp),
    ) {
        Row(
            horizontalArrangement = Arrangement.spacedBy(2.dp),
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 6.dp),
        ) {
            for (emoji in REACTION_CHOICES) {
                val selected = emoji == selectedEmoji
                Box(
                    modifier = Modifier
                        .size(40.dp)
                        .clip(CircleShape)
                        .then(
                            if (selected) {
                                Modifier.background(MaterialTheme.colorScheme.primaryContainer)
                            } else {
                                Modifier
                            },
                        )
                        .clickable { onReact(emoji) }
                        .semantics {
                            contentDescription = if (selected) {
                                "React $emoji, selected. Tap to remove"
                            } else {
                                "React $emoji"
                            }
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(emoji, style = MaterialTheme.typography.titleLarge)
                }
            }
        }
    }
}

@Composable
private fun MessageActionPanel(
    canCopy: Boolean,
    onCopy: () -> Unit,
    onInfo: () -> Unit,
) {
    Surface(
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        tonalElevation = 6.dp,
        shadowElevation = 6.dp,
        modifier = Modifier
            .padding(top = 6.dp)
            // DropdownMenuItem fills whatever width it's given -- without a max
            // here (normally supplied by DropdownMenu's own popup sizing) it
            // would stretch to the full screen width instead of hugging its text.
            .widthIn(min = 176.dp, max = 220.dp),
    ) {
        Column(modifier = Modifier.padding(vertical = 6.dp)) {
            DropdownMenuItem(
                text = { Text("Copy") },
                enabled = canCopy,
                onClick = onCopy,
            )
            DropdownMenuItem(
                text = { Text("Info") },
                onClick = onInfo,
            )
        }
    }
}
