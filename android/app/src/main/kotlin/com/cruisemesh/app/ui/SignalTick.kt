package com.cruisemesh.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.chat.TickStatus

fun tickContentDescription(status: TickStatus): String = when (status) {
    TickStatus.SENT -> "Sent"
    TickStatus.DELIVERED -> "Delivered"
    TickStatus.READ -> "Read"
}

fun tickLegendText(status: TickStatus): String = when (status) {
    TickStatus.SENT -> "Sent: queued for delivery."
    TickStatus.DELIVERED -> "Delivered: received by the contact's device."
    TickStatus.READ -> "Read: viewed by the contact."
}

@Composable
fun SignalTick(
    status: TickStatus,
    tint: Color,
    bubbleColor: Color,
    modifier: Modifier = Modifier
) {
    Canvas(
        modifier = modifier
            .size(width = 22.dp, height = 14.dp)
            .semantics { contentDescription = tickContentDescription(status) }
    ) {
        val radius = 4.8.dp.toPx()
        val strokeWidth = 1.15.dp.toPx()
        val leftCenter = center.copy(x = center.x - 3.7.dp.toPx())
        val rightCenter = center.copy(x = center.x + 3.7.dp.toPx())

        fun drawCheck(checkCenter: Offset, color: Color) {
            val path = Path().apply {
                moveTo(checkCenter.x - radius * 0.42f, checkCenter.y + radius * 0.08f)
                lineTo(checkCenter.x - radius * 0.08f, checkCenter.y + radius * 0.4f)
                lineTo(checkCenter.x + radius * 0.54f, checkCenter.y - radius * 0.42f)
            }
            drawPath(
                path = path,
                color = color,
                style = Stroke(width = strokeWidth, cap = StrokeCap.Round, join = StrokeJoin.Round),
            )
        }

        fun drawOutlined(center: Offset) {
            drawCircle(color = bubbleColor, radius = radius, center = center)
            drawCircle(
                color = tint,
                radius = radius,
                center = center,
                style = Stroke(width = strokeWidth),
            )
            drawCheck(center, tint)
        }

        fun drawFilled(center: Offset) {
            drawCircle(color = tint, radius = radius, center = center)
            drawCheck(center, bubbleColor)
        }

        when (status) {
            TickStatus.SENT -> drawOutlined(rightCenter)
            TickStatus.DELIVERED -> {
                drawOutlined(leftCenter)
                drawOutlined(rightCenter)
            }
            TickStatus.READ -> {
                drawFilled(leftCenter)
                drawFilled(rightCenter)
            }
        }
    }
}

@Preview(showBackground = true)
@Composable
private fun SignalTickPreview() {
    CruiseMeshTheme {
        Row(modifier = Modifier.padding(16.dp)) {
            SignalTick(
                status = TickStatus.SENT,
                tint = MaterialTheme.colorScheme.onSurface,
                bubbleColor = MaterialTheme.colorScheme.surface,
            )
            Spacer(modifier = Modifier.size(12.dp))
            SignalTick(
                status = TickStatus.DELIVERED,
                tint = MaterialTheme.colorScheme.onSurface,
                bubbleColor = MaterialTheme.colorScheme.surface,
            )
            Spacer(modifier = Modifier.size(12.dp))
            SignalTick(
                status = TickStatus.READ,
                tint = MaterialTheme.colorScheme.primary,
                bubbleColor = MaterialTheme.colorScheme.surface,
            )
        }
    }
}
