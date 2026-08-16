package com.cruisemesh.app.ui

import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.CoreSailChecklistItem
import uniffi.cruisemesh_core.CoreSailChecklistItemId
import uniffi.cruisemesh_core.CoreSailChecklistReport
import uniffi.cruisemesh_core.CoreSailPermission
import uniffi.cruisemesh_core.CoreSailPermissionRow

/** How far the home-screen card has got, for [SailChecklistCard]. */
data class SailChecklistProgress(val doneCount: Int, val totalCount: Int)

/**
 * The "before you sail" checklist: the handful of things a family should
 * finish while they are still in the same room.
 *
 * A list, not a wizard. Every step ticks itself off from something the app
 * already knows, so it can be left half-done, closed, and picked up later; it
 * never blocks anything and it never asks when the trip is.
 *
 * The screen renders whatever the core hands it, in the order it hands it, and
 * decides nothing: which steps exist, which are optional, which are done and
 * whether the family is ready all come from `core_sail_checklist`. The
 * permission sub-rows likewise come from the report already filtered to this
 * platform's grants, so nothing here needs to know which platform it is on.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SailChecklistScreen(
    report: CoreSailChecklistReport,
    contactCount: Int,
    onShorePass: () -> Unit,
    onAddFamily: () -> Unit,
    onGrantPermission: (CoreSailPermission) -> Unit,
    onBackUp: () -> Unit,
    onBack: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_before_you_sail)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.ui_back),
                        )
                    }
                },
            )
        },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
        ) {
            Text(
                stringResource(R.string.ui_before_you_sail_intro),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(12.dp))
            if (report.ready) {
                Surface(
                    modifier = Modifier.fillMaxWidth(),
                    shape = RoundedCornerShape(12.dp),
                    color = MaterialTheme.colorScheme.primaryContainer,
                    contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                ) {
                    Text(
                        stringResource(R.string.ui_before_you_sail_ready),
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
                    )
                }
            } else {
                Text(
                    stringResource(
                        R.string.ui_before_you_sail_progress,
                        report.doneCount.toInt(),
                        report.totalCount.toInt(),
                    ),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            Spacer(Modifier.height(8.dp))

            report.items.forEach { item ->
                SailChecklistRow(
                    item = item,
                    contactCount = contactCount,
                    onClick = when (item.id) {
                        CoreSailChecklistItemId.SHORE_PASS -> onShorePass
                        CoreSailChecklistItemId.ADD_FAMILY -> onAddFamily
                        CoreSailChecklistItemId.BACKUP -> onBackUp
                        // The permissions item is opened one grant at a time
                        // through its own sub-rows below, and the offline test
                        // is something two people do with two phones -- there
                        // is no screen either row could honestly open.
                        CoreSailChecklistItemId.PERMISSIONS,
                        CoreSailChecklistItemId.OFFLINE_TEST,
                        -> null
                    },
                )
                if (item.id == CoreSailChecklistItemId.PERMISSIONS) {
                    report.permissions.forEach { permissionRow ->
                        SailPermissionRow(
                            row = permissionRow,
                            onClick = { onGrantPermission(permissionRow.permission) },
                        )
                    }
                }
            }
        }
    }
}

/**
 * The dismissible home-screen entry point: how far along the checklist is,
 * and a way in.
 *
 * The caller decides whether it appears at all -- it is hidden once the family
 * is ready, so nobody is left staring at a total made up partly of steps they
 * never meant to do.
 */
@Composable
fun SailChecklistCard(
    progress: SailChecklistProgress,
    onClick: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.tertiaryContainer,
        contentColor = MaterialTheme.colorScheme.onTertiaryContainer,
    ) {
        Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text(
                stringResource(
                    R.string.ui_before_you_sail_card_progress,
                    progress.doneCount,
                    progress.totalCount,
                ),
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                stringResource(R.string.ui_before_you_sail_card_body),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(top = 2.dp),
            )
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.align(Alignment.End),
            ) {
                Text(stringResource(R.string.ui_dismiss))
            }
        }
    }
}

@Composable
private fun SailChecklistRow(
    item: CoreSailChecklistItem,
    contactCount: Int,
    onClick: (() -> Unit)?,
) {
    val rowModifier = Modifier
        .fillMaxWidth()
        .let { if (onClick != null) it.clickable(onClick = onClick) else it }
        .padding(vertical = 10.dp)
    Row(modifier = rowModifier, verticalAlignment = Alignment.Top) {
        SailStepMark(done = item.done)
        Column(modifier = Modifier.padding(start = 12.dp)) {
            Text(sailItemTitle(item.id), style = MaterialTheme.typography.bodyLarge)
            Text(
                sailItemDetail(item.id),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 2.dp),
            )
            if (item.id == CoreSailChecklistItemId.ADD_FAMILY && contactCount > 0) {
                Text(
                    pluralStringResource(R.plurals.ui_sail_family_added, contactCount, contactCount),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
            if (!item.required) {
                Text(
                    stringResource(R.string.ui_sail_step_optional),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
        }
    }
}

@Composable
private fun SailPermissionRow(row: CoreSailPermissionRow, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            // Indented under the permissions step: these are its parts, not
            // five top-level steps.
            .padding(start = 32.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.Top,
    ) {
        SailStepMark(done = row.granted)
        Column(modifier = Modifier.padding(start = 12.dp)) {
            Text(sailPermissionTitle(row.permission), style = MaterialTheme.typography.bodyMedium)
            Text(
                sailPermissionDetail(row.permission),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
    }
}

/**
 * The tick, or the empty circle waiting for one.
 *
 * Two different shapes rather than two tints of the same one, so the state
 * still reads for anyone who cannot tell the colours apart, and each carries
 * its own screen-reader label.
 */
@Composable
private fun SailStepMark(done: Boolean) {
    if (done) {
        Icon(
            Icons.Filled.CheckCircle,
            contentDescription = stringResource(R.string.ui_sail_step_done),
            tint = MaterialTheme.colorScheme.primary,
            modifier = Modifier.size(20.dp),
        )
    } else {
        val label = stringResource(R.string.ui_sail_step_not_done)
        Spacer(
            modifier = Modifier
                .size(20.dp)
                .border(2.dp, MaterialTheme.colorScheme.outline, CircleShape)
                .semantics { contentDescription = label },
        )
    }
}

@Composable
private fun sailItemTitle(id: CoreSailChecklistItemId): String = stringResource(
    when (id) {
        CoreSailChecklistItemId.SHORE_PASS -> R.string.ui_sail_shore_pass
        CoreSailChecklistItemId.ADD_FAMILY -> R.string.ui_sail_add_family
        CoreSailChecklistItemId.PERMISSIONS -> R.string.ui_sail_permissions
        CoreSailChecklistItemId.OFFLINE_TEST -> R.string.ui_sail_offline_test
        CoreSailChecklistItemId.BACKUP -> R.string.ui_sail_backup
    },
)

@Composable
private fun sailItemDetail(id: CoreSailChecklistItemId): String = stringResource(
    when (id) {
        CoreSailChecklistItemId.SHORE_PASS -> R.string.ui_sail_shore_pass_detail
        CoreSailChecklistItemId.ADD_FAMILY -> R.string.ui_sail_add_family_detail
        CoreSailChecklistItemId.PERMISSIONS -> R.string.ui_sail_permissions_detail
        CoreSailChecklistItemId.OFFLINE_TEST -> R.string.ui_sail_offline_test_detail
        CoreSailChecklistItemId.BACKUP -> R.string.ui_sail_backup_detail
    },
)

@Composable
private fun sailPermissionTitle(permission: CoreSailPermission): String = stringResource(
    when (permission) {
        CoreSailPermission.BLUETOOTH -> R.string.ui_sail_permission_bluetooth
        CoreSailPermission.NOTIFICATIONS -> R.string.ui_sail_permission_notifications
        CoreSailPermission.BATTERY_OPTIMIZATION -> R.string.ui_sail_permission_battery
    },
)

@Composable
private fun sailPermissionDetail(permission: CoreSailPermission): String = stringResource(
    when (permission) {
        CoreSailPermission.BLUETOOTH -> R.string.ui_sail_permission_bluetooth_detail
        CoreSailPermission.NOTIFICATIONS -> R.string.ui_sail_permission_notifications_detail
        CoreSailPermission.BATTERY_OPTIMIZATION -> R.string.ui_sail_permission_battery_detail
    },
)

@Preview(showBackground = true)
@Composable
private fun SailChecklistScreenPreview() {
    val items = listOf(
        CoreSailChecklistItem(CoreSailChecklistItemId.SHORE_PASS, required = false, done = true),
        CoreSailChecklistItem(CoreSailChecklistItemId.ADD_FAMILY, required = true, done = true),
        CoreSailChecklistItem(CoreSailChecklistItemId.PERMISSIONS, required = true, done = false),
        CoreSailChecklistItem(CoreSailChecklistItemId.OFFLINE_TEST, required = true, done = false),
        CoreSailChecklistItem(CoreSailChecklistItemId.BACKUP, required = false, done = false),
    )
    CruiseMeshTheme {
        SailChecklistScreen(
            report = CoreSailChecklistReport(
                items = items,
                permissions = listOf(
                    CoreSailPermissionRow(CoreSailPermission.BLUETOOTH, granted = true),
                    CoreSailPermissionRow(CoreSailPermission.NOTIFICATIONS, granted = false),
                    CoreSailPermissionRow(CoreSailPermission.BATTERY_OPTIMIZATION, granted = false),
                ),
                ready = false,
                doneCount = 2u,
                totalCount = 5u,
                requiredDoneCount = 1u,
                requiredTotalCount = 3u,
            ),
            contactCount = 3,
            onShorePass = {},
            onAddFamily = {},
            onGrantPermission = {},
            onBackUp = {},
            onBack = {},
        )
    }
}
