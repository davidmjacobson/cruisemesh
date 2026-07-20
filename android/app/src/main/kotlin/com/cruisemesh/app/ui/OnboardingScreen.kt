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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

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
                            Text(stringResource(R.string.ui_back))
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
                        Text(
                            stringResource(
                                if (isLastPage) R.string.ui_start_using_cruisemesh else R.string.ui_next,
                            ),
                        )
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
                Text(text = stringResource(R.string.ui_cruisemesh_setup),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    text = stringResource(R.string.ui_step_of, page + 1, pages),
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
        title = "Messages that find a way through",
        body = "CruiseMesh delivers messages to people nearby even without Wi-Fi or cell service — using Bluetooth, local Wi-Fi, or hopping phone to phone.",
    ) {
        TextButton(
            onClick = onRestore,
            modifier = Modifier.padding(top = 8.dp),
        ) {
            Text(stringResource(R.string.ui_already_set_up_restore_from_a_backup))
        }
    }
}

@Composable
private fun DeliverySlide() {
    SlideScaffold(
        eyebrow = "How it works",
        title = "It uses whatever's around",
        body = "Nearby, messages travel phone-to-phone over Bluetooth and Wi-Fi. Farther away, they hop between other CruiseMesh phones until they reach your friend.",
    ) {
        HighlightCard(
            title = "Private by default",
            detail = "Always end-to-end encrypted — even the phones that help carry a message can't read it.",
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
        eyebrow = "Turn on a few permissions",
        title = "Give CruiseMesh a way to reach people",
        body = "Each of these opens up another way for your messages to get through.",
    ) {
        if (!meshPermissionsGranted) {
            Surface(
                shape = RoundedCornerShape(18.dp),
                color = MaterialTheme.colorScheme.tertiaryContainer,
                contentColor = MaterialTheme.colorScheme.onTertiaryContainer,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 4.dp),
            ) {
                Text(text = stringResource(R.string.ui_nearby_permissions_are_required_skipping_them_means_the),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(16.dp),
                )
            }
        }

        val items = listOf(
            PermissionItem(
                title = "Nearby devices and notifications",
                detail = "Lets CruiseMesh scan for and connect to phones around you, and notify you when a message arrives.",
                enabled = meshPermissionsGranted,
                required = true,
            ),
            PermissionItem(
                title = "Background activity",
                detail = "Keeps the mesh working while your phone is in your pocket.",
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
            Text(
                stringResource(
                    if (meshPermissionsGranted) R.string.ui_nearby_access_enabled
                    else R.string.ui_enable_nearby_access_required,
                ),
            )
        }

        OutlinedButton(
            onClick = onRequestBatteryExemption,
            enabled = !batteryExemptionGranted,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 12.dp),
        ) {
            Text(
                stringResource(
                    if (batteryExemptionGranted) R.string.ui_background_activity_enabled
                    else R.string.ui_enable_background_activity,
                ),
            )
        }

        Text(
            text = stringResource(
                if (meshPermissionsGranted) R.string.ui_nearby_access_on_detail
                else R.string.ui_nearby_access_off_detail,
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
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
            missingRequired -> MaterialTheme.colorScheme.tertiaryContainer.copy(alpha = 0.85f)
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
                    item.required -> "Needed to send messages"
                    else -> "Recommended"
                },
                style = MaterialTheme.typography.labelLarge,
                color = when {
                    item.enabled -> MaterialTheme.colorScheme.primary
                    item.required -> MaterialTheme.colorScheme.onTertiaryContainer
                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier.padding(top = 10.dp),
            )
        }
    }
}
