package com.cruisemesh.app.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

private data class PermissionItem(
    val title: String,
    val detail: String,
    val enabled: Boolean,
    val required: Boolean = false,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun OnboardingScreen(
    userId: ByteArray,
    displayId: String,
    displayName: String,
    avatarPath: String?,
    meshPermissionsGranted: Boolean,
    batteryExemptionGranted: Boolean,
    onDisplayNameChange: (String) -> Unit,
    onTakePhoto: () -> Unit,
    onChoosePhoto: () -> Unit,
    onRemovePhoto: () -> Unit,
    onRequestMeshPermissions: () -> Unit,
    onRequestBatteryExemption: () -> Unit,
    onRestore: () -> Unit,
    onComplete: () -> Unit,
) {
    var page by rememberSaveable { mutableStateOf(0) }
    val pages = 4
    val canGoBack = page > 0
    val isLastPage = page == pages - 1

    Scaffold(
        containerColor = Color.Transparent,
        bottomBar = {
            Surface(
                tonalElevation = 2.dp,
                color = MaterialTheme.colorScheme.surface.copy(alpha = 0.96f),
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 20.dp, vertical = 16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (canGoBack) {
                        TextButton(onClick = { page -= 1 }) {
                            Text("Back")
                        }
                    } else {
                        Spacer(modifier = Modifier.height(1.dp))
                    }
                    Button(onClick = {
                        if (isLastPage) {
                            onComplete()
                        } else {
                            page += 1
                        }
                    }) {
                        Text(if (isLastPage) "Start using CruiseMesh" else "Next")
                    }
                }
            }
        },
    ) { innerPadding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(
                    Brush.verticalGradient(
                        colors = listOf(
                            MaterialTheme.colorScheme.primary.copy(alpha = 0.18f),
                            MaterialTheme.colorScheme.tertiary.copy(alpha = 0.10f),
                            MaterialTheme.colorScheme.background,
                        ),
                    ),
                ),
        ) {
            Box(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .size(180.dp)
                    .padding(top = 12.dp, end = 12.dp)
                    .background(
                        brush = Brush.radialGradient(
                            colors = listOf(
                                MaterialTheme.colorScheme.secondary.copy(alpha = 0.16f),
                                Color.Transparent,
                            ),
                        ),
                        shape = CircleShape,
                    ),
            )

            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .padding(horizontal = 24.dp, vertical = 20.dp),
            ) {
                Text(
                    text = "CruiseMesh setup",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    text = "Step ${page + 1} of $pages",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
                Row(
                    modifier = Modifier.padding(top = 16.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    repeat(pages) { index ->
                        Surface(
                            modifier = Modifier.size(width = if (index == page) 28.dp else 10.dp, height = 10.dp),
                            shape = RoundedCornerShape(999.dp),
                            color = if (index == page) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.primary.copy(alpha = 0.22f)
                            },
                        ) {}
                    }
                }

                AnimatedContent(
                    targetState = page,
                    transitionSpec = { fadeIn() togetherWith fadeOut() },
                    label = "onboarding_page",
                ) { currentPage ->
                    Surface(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(top = 24.dp),
                        shape = RoundedCornerShape(28.dp),
                        color = MaterialTheme.colorScheme.surface.copy(alpha = 0.94f),
                        tonalElevation = 2.dp,
                    ) {
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .verticalScroll(rememberScrollState())
                                .padding(24.dp),
                        ) {
                            when (currentPage) {
                                0 -> WelcomeSlide(onRestore = onRestore)
                                1 -> DeliverySlide()
                                2 -> PermissionsSlide(
                                    meshPermissionsGranted = meshPermissionsGranted,
                                    batteryExemptionGranted = batteryExemptionGranted,
                                    onRequestMeshPermissions = onRequestMeshPermissions,
                                    onRequestBatteryExemption = onRequestBatteryExemption,
                                )
                                else -> ProfileSlide(
                                    userId = userId,
                                    displayId = displayId,
                                    displayName = displayName,
                                    avatarPath = avatarPath,
                                    onDisplayNameChange = onDisplayNameChange,
                                    onTakePhoto = onTakePhoto,
                                    onChoosePhoto = onChoosePhoto,
                                    onRemovePhoto = onRemovePhoto,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun WelcomeSlide(onRestore: () -> Unit) {
    SlideScaffold(
        eyebrow = "Nearby-first messaging",
        title = "Welcome to CruiseMesh",
        body = "CruiseMesh helps you communicate with friends and family nearby, even when you do not have Wi-Fi or cell service.",
    ) {
        HighlightCard(
            title = "Built for the moments when networks disappear",
            detail = "Keep conversations moving on hikes, cruises, festivals, road trips, and anywhere coverage is unreliable.",
        )
        TextButton(
            onClick = onRestore,
            modifier = Modifier.padding(top = 8.dp),
        ) {
            Text("Already set up? Restore from a backup")
        }
    }
}

@Composable
private fun DeliverySlide() {
    SlideScaffold(
        eyebrow = "How it works",
        title = "Messages can hop phone to phone",
        body = "CruiseMesh uses your phone and other phones running CruiseMesh to help deliver messages, even when you are too far from your friend to connect to their phone directly over Bluetooth.",
    ) {
        HighlightCard(
            title = "Private by default",
            detail = "Your messages are encrypted end to end, so nearby relays help carry them without being able to read them.",
        )
    }
}

@Composable
private fun PermissionsSlide(
    meshPermissionsGranted: Boolean,
    batteryExemptionGranted: Boolean,
    onRequestMeshPermissions: () -> Unit,
    onRequestBatteryExemption: () -> Unit,
) {
    SlideScaffold(
        eyebrow = "Required for messaging",
        title = "CruiseMesh needs these permissions to work",
        body = "Without Nearby devices access, this app cannot scan, connect, send, or receive messages. It will not work as designed until you grant them.",
    ) {
        if (!meshPermissionsGranted) {
            Surface(
                shape = RoundedCornerShape(18.dp),
                color = MaterialTheme.colorScheme.errorContainer,
                contentColor = MaterialTheme.colorScheme.onErrorContainer,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 4.dp),
            ) {
                Text(
                    text = "Nearby permissions are required. Skipping them means the mesh stays off.",
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(16.dp),
                )
            }
        }

        val items = listOf(
            PermissionItem(
                title = "Nearby devices and notifications",
                detail = "Required so CruiseMesh can scan, advertise, connect over Bluetooth, and notify you when messages arrive.",
                enabled = meshPermissionsGranted,
                required = true,
            ),
            PermissionItem(
                title = "Background activity",
                detail = "Strongly recommended so the mesh can keep syncing while your phone is in your pocket.",
                enabled = batteryExemptionGranted,
                required = false,
            ),
        )
        items.forEach { item -> PermissionStatusCard(item) }

        Button(
            onClick = onRequestMeshPermissions,
            enabled = !meshPermissionsGranted,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 18.dp),
        ) {
            Text(if (meshPermissionsGranted) "Nearby access enabled" else "Enable nearby access (required)")
        }

        OutlinedButton(
            onClick = onRequestBatteryExemption,
            enabled = !batteryExemptionGranted,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 12.dp),
        ) {
            Text(if (batteryExemptionGranted) "Background activity enabled" else "Enable background activity")
        }

        Text(
            text = if (meshPermissionsGranted) {
                "Nearby access is on. Background exemption is optional but makes delivery more reliable when the app is not open."
            } else {
                "You can finish setup without granting access, but the home screen will keep warning you — and no messages will move over the mesh until you do."
            },
            style = MaterialTheme.typography.bodySmall,
            color = if (meshPermissionsGranted) {
                MaterialTheme.colorScheme.onSurfaceVariant
            } else {
                MaterialTheme.colorScheme.error
            },
            modifier = Modifier.padding(top = 14.dp),
        )
    }
}

@Composable
private fun ProfileSlide(
    userId: ByteArray,
    displayId: String,
    displayName: String,
    avatarPath: String?,
    onDisplayNameChange: (String) -> Unit,
    onTakePhoto: () -> Unit,
    onChoosePhoto: () -> Unit,
    onRemovePhoto: () -> Unit,
) {
    SlideScaffold(
        eyebrow = "Your profile",
        title = "What name would you like to go by?",
        body = "This is what people will see when you share your friend card or add each other nearby. You can change it later.",
    ) {
        LocalProfileEditor(
            userId = userId,
            displayId = displayId,
            displayName = displayName,
            avatarPath = avatarPath,
            onDisplayNameChange = onDisplayNameChange,
            onTakePhoto = onTakePhoto,
            onChoosePhoto = onChoosePhoto,
            onRemovePhoto = onRemovePhoto,
            helperText = "Your photo is shared with friends after you connect.",
        )

        Text(
            text = "Device ID: $displayId",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 18.dp),
        )
    }
}

@Composable
private fun SlideScaffold(
    eyebrow: String,
    title: String,
    body: String,
    content: @Composable () -> Unit,
) {
    Text(
        text = eyebrow,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
    )
    Text(
        text = title,
        style = MaterialTheme.typography.headlineMedium.copy(fontWeight = FontWeight.SemiBold),
        modifier = Modifier.padding(top = 8.dp),
    )
    Text(
        text = body,
        style = MaterialTheme.typography.bodyLarge,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 16.dp),
    )
    Spacer(modifier = Modifier.height(24.dp))
    content()
}

@Composable
private fun HighlightCard(title: String, detail: String) {
    Surface(
        shape = RoundedCornerShape(22.dp),
        color = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.72f),
    ) {
        Column(modifier = Modifier.padding(20.dp)) {
            Text(
                text = title,
                style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold),
            )
            Text(
                text = detail,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.padding(top = 8.dp),
            )
        }
    }
}

@Composable
private fun PermissionStatusCard(item: PermissionItem) {
    val missingRequired = item.required && !item.enabled
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 12.dp),
        shape = RoundedCornerShape(20.dp),
        color = when {
            item.enabled -> MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.75f)
            missingRequired -> MaterialTheme.colorScheme.errorContainer.copy(alpha = 0.85f)
            else -> MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.7f)
        },
    ) {
        Column(modifier = Modifier.padding(18.dp)) {
            Text(
                text = item.title,
                style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold),
            )
            Text(
                text = item.detail,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 6.dp),
            )
            Text(
                text = when {
                    item.enabled -> "Enabled"
                    item.required -> "Required — mesh off without this"
                    else -> "Recommended"
                },
                style = MaterialTheme.typography.labelLarge,
                color = when {
                    item.enabled -> MaterialTheme.colorScheme.primary
                    item.required -> MaterialTheme.colorScheme.error
                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier.padding(top = 10.dp),
            )
        }
    }
}
