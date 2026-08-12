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
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
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
import androidx.compose.ui.platform.testTag
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
    notificationPermissionGranted: Boolean,
    batteryExemptionGranted: Boolean,
    onDisplayNameChange: (String) -> Unit,
    onTakePhoto: () -> Unit,
    onChoosePhoto: () -> Unit,
    onRemovePhoto: () -> Unit,
    onRequestMeshPermissions: () -> Unit,
    onRequestNotificationPermission: () -> Unit,
    onRequestBatteryExemption: () -> Unit,
    onRestore: () -> Unit,
    onComplete: () -> Unit,
) {
    var page by rememberSaveable { mutableStateOf(0) }
    val pages = 5
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
                        // The activity draws edge to edge and Scaffold does not
                        // inset a custom bottom bar, so without this the buttons
                        // sit underneath the system navigation bar.
                        .windowInsetsPadding(WindowInsets.navigationBars)
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
                    Button(
                        onClick = {
                            if (isLastPage) {
                                onComplete()
                            } else {
                                page += 1
                            }
                        },
                        // A name is required to finish. Nothing is substituted
                        // if it is left blank, so the gate has to be here.
                        enabled = !isLastPage || displayName.isNotBlank(),
                    ) {
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
                .testTag(UiTestTags.ONBOARDING_SCREEN)
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
                                    notificationPermissionGranted = notificationPermissionGranted,
                                    batteryExemptionGranted = batteryExemptionGranted,
                                    onRequestMeshPermissions = onRequestMeshPermissions,
                                    onRequestNotificationPermission = onRequestNotificationPermission,
                                    onRequestBatteryExemption = onRequestBatteryExemption,
                                )
                                3 -> WifiSlide()
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
        eyebrow = stringResource(R.string.ui_onboarding_welcome_eyebrow),
        title = stringResource(R.string.ui_onboarding_welcome_title),
        body = stringResource(R.string.ui_onboarding_welcome_body),
    ) {
        Text(
            stringResource(R.string.ui_onboarding_welcome_support),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 12.dp),
        )
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
        eyebrow = stringResource(R.string.ui_onboarding_delivery_eyebrow),
        title = stringResource(R.string.ui_onboarding_delivery_title),
        body = stringResource(R.string.ui_onboarding_delivery_body),
    ) {
        HighlightCard(
            title = stringResource(R.string.ui_onboarding_private_title),
            detail = stringResource(R.string.ui_onboarding_private_detail),
        )
    }
}

/**
 * T5 slide 4. The single least guessable thing about running CruiseMesh:
 * staying joined to a Wi-Fi network that has no internet is *useful*, because
 * the local network reaches nearby phones faster than Bluetooth does. Says so
 * plainly, and pre-empts the obvious worry that a dead connection will be
 * treated as a working one.
 */
@Composable
private fun WifiSlide() {
    SlideScaffold(
        eyebrow = stringResource(R.string.ui_onboarding_wifi_eyebrow),
        title = stringResource(R.string.ui_onboarding_wifi_title),
        body = stringResource(R.string.ui_onboarding_wifi_body),
    ) {
        HighlightCard(
            title = stringResource(R.string.ui_onboarding_wifi_support_title),
            detail = stringResource(R.string.ui_onboarding_wifi_support_detail),
        )
    }
}

@Composable
private fun PermissionsSlide(
    meshPermissionsGranted: Boolean,
    notificationPermissionGranted: Boolean,
    batteryExemptionGranted: Boolean,
    onRequestMeshPermissions: () -> Unit,
    onRequestNotificationPermission: () -> Unit,
    onRequestBatteryExemption: () -> Unit,
) {
    SlideScaffold(
        eyebrow = stringResource(R.string.ui_onboarding_permissions_eyebrow),
        title = stringResource(R.string.ui_onboarding_permissions_title),
        body = stringResource(R.string.ui_onboarding_permissions_body),
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
                title = stringResource(R.string.ui_onboarding_permission_nearby_title),
                detail = stringResource(R.string.ui_onboarding_permission_nearby_detail),
                enabled = meshPermissionsGranted,
                required = true,
            ),
            PermissionItem(
                title = stringResource(R.string.ui_onboarding_permission_notifications_title),
                detail = stringResource(R.string.ui_onboarding_permission_notifications_detail),
                enabled = notificationPermissionGranted,
            ),
            PermissionItem(
                title = stringResource(R.string.ui_onboarding_permission_background_title),
                detail = stringResource(R.string.ui_onboarding_permission_background_detail),
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
            onClick = onRequestNotificationPermission,
            enabled = !notificationPermissionGranted,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 12.dp),
        ) {
            Text(
                stringResource(
                    if (notificationPermissionGranted) R.string.ui_notifications_enabled
                    else R.string.ui_enable_notifications,
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

        // Radios are the subject of this slide, and airplane mode is the one
        // radio setting that decides whether the trip ends with a roaming
        // bill. Same supporting-text style as the line above it.
        Text(
            text = stringResource(R.string.ui_onboarding_airplane_tip),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 10.dp),
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
        eyebrow = stringResource(R.string.ui_onboarding_profile_eyebrow),
        title = stringResource(R.string.ui_onboarding_profile_title),
        body = stringResource(R.string.ui_onboarding_profile_body),
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
            nameError = if (displayName.isBlank()) {
                stringResource(R.string.ui_onboarding_name_required)
            } else {
                null
            },
            // Says why the button below is disabled; without it a blank field
            // and a dead button is a dead end.
            helperText = stringResource(R.string.ui_onboarding_profile_photo_helper),
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
