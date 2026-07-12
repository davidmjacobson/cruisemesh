package com.cruisemesh.app.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.mesh.ReachabilityLevel

/** How urgent a home-screen connectivity callout is. */
enum class ConnectivityWarningSeverity {
    /** Mesh cannot send/receive as designed (missing permissions, BT off). */
    Blocking,
    /** Mesh may still run, but something is degraded. */
    Caution,
}

/**
 * Structured home-screen callout when connectivity is broken or degraded.
 * Prefer this over a single soft status line so users understand the app
 * will not work as designed until they act.
 */
data class ConnectivityWarning(
    val title: String,
    val body: String,
    val actionLabel: String,
    val secondaryActionLabel: String? = null,
    val severity: ConnectivityWarningSeverity = ConnectivityWarningSeverity.Blocking,
)

/**
 * Prominent banner above the mesh status pill when the mesh cannot (or may
 * not) work as designed. Blocking severity uses error colors so it does not
 * blend into routine "Mesh stopped" status.
 */
@Composable
fun ConnectivityWarningBanner(
    warning: ConnectivityWarning,
    onClick: () -> Unit,
    onSecondaryClick: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    val containerColor = when (warning.severity) {
        ConnectivityWarningSeverity.Blocking -> MaterialTheme.colorScheme.errorContainer
        ConnectivityWarningSeverity.Caution -> MaterialTheme.colorScheme.tertiaryContainer
    }
    val contentColor = when (warning.severity) {
        ConnectivityWarningSeverity.Blocking -> MaterialTheme.colorScheme.onErrorContainer
        ConnectivityWarningSeverity.Caution -> MaterialTheme.colorScheme.onTertiaryContainer
    }
    val buttonContainer = when (warning.severity) {
        ConnectivityWarningSeverity.Blocking -> MaterialTheme.colorScheme.error
        ConnectivityWarningSeverity.Caution -> MaterialTheme.colorScheme.tertiary
    }
    val buttonContent = when (warning.severity) {
        ConnectivityWarningSeverity.Blocking -> MaterialTheme.colorScheme.onError
        ConnectivityWarningSeverity.Caution -> MaterialTheme.colorScheme.onTertiary
    }

    Surface(
        modifier = modifier
            .fillMaxWidth()
            .semantics {
                role = Role.Button
                contentDescription = "${warning.title}. ${warning.body}"
            }
            .clickable(onClick = onClick),
        color = containerColor,
        contentColor = contentColor,
        shape = RoundedCornerShape(0.dp),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 14.dp),
        ) {
            Row(verticalAlignment = Alignment.Top) {
                Icon(
                    imageVector = Icons.Default.Warning,
                    contentDescription = null,
                    modifier = Modifier
                        .padding(top = 2.dp)
                        .size(22.dp),
                )
                Spacer(modifier = Modifier.width(10.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = warning.title,
                        style = MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = warning.body,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 4.dp),
                        maxLines = 4,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            Button(
                onClick = onClick,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 12.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = buttonContainer,
                    contentColor = buttonContent,
                ),
            ) {
                Text(warning.actionLabel)
            }
            if (warning.secondaryActionLabel != null && onSecondaryClick != null) {
                TextButton(
                    onClick = onSecondaryClick,
                    modifier = Modifier.fillMaxWidth(),
                    colors = ButtonDefaults.textButtonColors(contentColor = contentColor),
                ) {
                    Text(warning.secondaryActionLabel)
                }
            }
        }
    }
}

@Composable
fun MeshStatusLegendDialog(
    statusText: String,
    canStartMesh: Boolean,
    onStartMesh: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Mesh status") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    statusText,
                    style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold),
                )
                Text(
                    "Open the app when you sit down with family so phones can sync over Bluetooth.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    LegendRow(ReachabilityLevel.NEARBY, "Nearby", "Direct Bluetooth link")
                    LegendRow(ReachabilityLevel.ONLINE_RELAY, "Relay", "Online through relay")
                    LegendRow(ReachabilityLevel.RECENT, "Recent", "Seen recently")
                    LegendRow(ReachabilityLevel.MESH_CARRY, "Carried", "Nearby phones may carry messages")
                    LegendRow(ReachabilityLevel.OFFLINE, "Offline", "No recent route")
                }
            }
        },
        confirmButton = {
            if (canStartMesh) {
                Button(
                    onClick = {
                        onDismiss()
                        onStartMesh()
                    },
                ) {
                    Text("Start mesh")
                }
            } else {
                TextButton(onClick = onDismiss) { Text("Done") }
            }
        },
        dismissButton = {
            if (canStartMesh) {
                TextButton(onClick = onDismiss) { Text("Cancel") }
            }
        },
    )
}

@Composable
private fun LegendRow(level: ReachabilityLevel, label: String, detail: String) {
    val palette = LocalReachabilityPalette.current
    val dotColor = when (level) {
        ReachabilityLevel.NEARBY -> palette.nearby
        ReachabilityLevel.ONLINE_RELAY -> palette.onlineRelay
        ReachabilityLevel.RECENT, ReachabilityLevel.MESH_CARRY -> palette.recent
        ReachabilityLevel.OFFLINE -> palette.neutral
    }
    val isHollow = level == ReachabilityLevel.MESH_CARRY
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(
            modifier = Modifier
                .size(14.dp)
                .clip(CircleShape)
                .background(if (isHollow) MaterialTheme.colorScheme.surface else dotColor)
                .border(2.dp, dotColor, CircleShape),
        )
        Spacer(modifier = Modifier.width(10.dp))
        Column {
            Text(label, style = MaterialTheme.typography.bodySmall.copy(fontWeight = FontWeight.SemiBold))
            Text(detail, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
fun MeshStatusPill(
    text: String,
    dotColor: Color?,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val shouldPulse = dotColor != null &&
        (text.startsWith("Mesh on") || text.contains("Starting", ignoreCase = true))
    val pulse = rememberInfiniteTransition(label = "mesh-status-pulse")
    val dotScale = if (shouldPulse) {
        pulse.animateFloat(
            initialValue = 0.82f,
            targetValue = 1.22f,
            animationSpec = infiniteRepeatable(
                animation = tween(durationMillis = 1400),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "mesh-status-dot-scale",
        ).value
    } else {
        1f
    }
    val dotAlpha = if (shouldPulse) {
        pulse.animateFloat(
            initialValue = 0.62f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(
                animation = tween(durationMillis = 1400),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "mesh-status-dot-alpha",
        ).value
    } else {
        1f
    }

    Surface(
        modifier = modifier
            .fillMaxWidth()
            .minimumInteractiveComponentSize()
            .semantics {
                role = Role.Button
                contentDescription = "Mesh status: $text"
            }
            .clickable(onClick = onClick),
        shape = CircleShape,
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        tonalElevation = 1.dp,
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (dotColor != null) {
                Box(
                    modifier = Modifier
                        .size(8.dp)
                        .graphicsLayer {
                            scaleX = dotScale
                            scaleY = dotScale
                            alpha = dotAlpha
                        }
                        .clip(CircleShape)
                        .background(dotColor),
                )
                Spacer(modifier = Modifier.width(8.dp))
            }
            Text(
                text = text,
                style = MaterialTheme.typography.labelLarge,
                textAlign = TextAlign.Center,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}
