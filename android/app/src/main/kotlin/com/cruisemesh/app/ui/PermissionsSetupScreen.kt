package com.cruisemesh.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R

/**
 * The permissions step on its own, for the routes that arrive past the wizard.
 *
 * "This is another of my devices" and "Restore from a backup" both finish by
 * marking setup complete from underneath: the phone holds a person's contacts,
 * groups and history, so the wizard genuinely has nothing left to ask it — and
 * for everything except permissions that is right. What it left behind was a
 * chat list with the mesh off behind a notice, on a phone that had never been
 * asked for Nearby devices access at all; a two-phone session had to grant it
 * by hand from system settings.
 *
 * Deliberately the same content as the wizard's own slide rather than a second
 * telling of it: [PermissionsSlide] is shared, so the two can never drift.
 */
@Composable
fun PermissionsSetupScreen(
    meshPermissionsGranted: Boolean,
    notificationPermissionGranted: Boolean,
    batteryExemptionGranted: Boolean,
    onRequestMeshPermissions: () -> Unit,
    onRequestNotificationPermission: () -> Unit,
    onRequestBatteryExemption: () -> Unit,
    onContinue: () -> Unit,
) {
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
                        // inset a custom bottom bar; without this the button
                        // sits underneath the system navigation bar.
                        .windowInsetsPadding(WindowInsets.navigationBars)
                        .padding(horizontal = 20.dp, vertical = 16.dp),
                    horizontalArrangement = Arrangement.End,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Button(onClick = onContinue) {
                        Text(stringResource(R.string.ui_start_using_cruisemesh))
                    }
                }
            }
        },
    ) { innerPadding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .testTag(UiTestTags.PERMISSIONS_SETUP_SCREEN)
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
                    text = stringResource(R.string.ui_cruisemesh_setup),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    text = stringResource(R.string.ui_permissions_setup_linked_note),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )

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
                        PermissionsSlide(
                            meshPermissionsGranted = meshPermissionsGranted,
                            notificationPermissionGranted = notificationPermissionGranted,
                            batteryExemptionGranted = batteryExemptionGranted,
                            onRequestMeshPermissions = onRequestMeshPermissions,
                            onRequestNotificationPermission = onRequestNotificationPermission,
                            onRequestBatteryExemption = onRequestBatteryExemption,
                        )
                    }
                }
            }
        }
    }
}
