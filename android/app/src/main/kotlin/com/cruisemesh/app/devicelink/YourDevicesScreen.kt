package com.cruisemesh.app.devicelink

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.R
import com.cruisemesh.app.identity.DeviceKeyStore
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.coreRosterDeviceIds

/**
 * "Your devices" (`specs/multi-device-v1.md` §13 WP6).
 *
 * The one screen that answers "which phones and tablets am I signed in on?",
 * and the door to both journeys that change the answer. It shows what the
 * person's own roster says and nothing it worked out for itself: which devices
 * are listed, which one is this one, which one approves new devices, and — from
 * this phone's own notes — what they have been called and when this phone first
 * saw them.
 *
 * Deliberately not behind Advanced. §13's product bar puts what a family needs
 * on the surface and capability behind the door; a person who loses a phone
 * needs to find this in Settings without being told where to look.
 */
data class YourDeviceListItem(
    val row: OwnDeviceRow,
    val name: String,
    val firstSeenMs: Long?,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun YourDevicesScreen(
    identity: Identity,
    onBack: () -> Unit,
    onAddDevice: () -> Unit,
) {
    val context = LocalContext.current
    // Bumped after a removal or a rename so the list is re-read from the store
    // rather than patched in place: the roster is the fact, and re-reading it is
    // how a removal that half-finished shows up as what actually happened.
    var revision by remember { mutableIntStateOf(0) }

    val roster = remember(identity.userId, revision) {
        runCatching { AppStore.get(context).ownRoster() }.getOrNull()
    }
    val ownDeviceId = remember(identity.userId, revision) {
        DeviceKeyStore.load(context)?.deviceId
    }
    val items = remember(roster, ownDeviceId, revision) {
        // A never-linked install has no roster to read, but the sentence above
        // the list still says "this is the only device signed in as you" -- so
        // the screen shows that device rather than an empty space under its own
        // claim. No badge and no Remove: nothing approves anything yet, and
        // there is nothing else to be left with.
        val rows = roster?.let {
            ownDeviceRows(coreRosterDeviceIds(it), it.approvingDeviceId, ownDeviceId)
        } ?: listOf(thisDeviceOnlyRow(ownDeviceId))
        rows.map { row ->
            YourDeviceListItem(
                row = row,
                name = DeviceNameStore.name(context, row.deviceIdHex).orEmpty(),
                // Read, never stamped: the stamp is written when a roster is
                // adopted or applied, so a device with no date is one this phone
                // met before it kept notes rather than one nobody has opened
                // this screen for.
                firstSeenMs = DeviceNameStore.firstSeenMs(context, row.deviceIdHex),
            )
        }
    }
    val shape = yourDevicesShape(roster != null, items.map { it.row })
    val canAdd = canAddDevice(roster != null, items.map { it.row })

    var renaming by remember { mutableStateOf<YourDeviceListItem?>(null) }
    var removing by remember { mutableStateOf<YourDeviceListItem?>(null) }
    var removalOutcome by remember { mutableStateOf<RemoveDeviceResult?>(null) }
    var removalRunning by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_your_devices)) },
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
        YourDevicesContent(
            shape = shape,
            items = items,
            canAddDevice = canAdd,
            modifier = Modifier.padding(innerPadding),
            onAddDevice = onAddDevice,
            onRename = { renaming = it },
            onRemove = { removing = it },
        )
    }

    renaming?.let { item ->
        RenameDeviceDialog(
            item = item,
            onDismiss = { renaming = null },
            onSave = { name ->
                DeviceNameStore.setName(context, item.row.deviceIdHex, name)
                renaming = null
                revision += 1
            },
        )
    }

    removing?.let { item ->
        RemoveDeviceDialog(
            item = item,
            onDismiss = { removing = null },
            onConfirm = {
                removing = null
                removalRunning = true
                // Off the main thread: §10.1's commit re-seals the retained
                // backlog record by record before it adopts anything, so on a
                // fleet that has been dark for a fortnight this is real work and
                // not a flag being set.
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        RemoveDeviceSession(context, identity).remove(item.row.deviceId)
                    }
                    removalRunning = false
                    removalOutcome = outcome
                    revision += 1
                }
            },
        )
    }

    if (removalRunning) {
        // Not dismissible: the ceremony is mid-flight and there is nothing
        // useful a person could do to it from here.
        AlertDialog(
            onDismissRequest = {},
            title = { Text(stringResource(R.string.ui_remove_this_device_question)) },
            text = { Text(stringResource(R.string.ui_removing_device)) },
            confirmButton = {},
        )
    }

    removalOutcome?.let { outcome ->
        RemovalOutcomeDialog(outcome = outcome, onDismiss = { removalOutcome = null })
    }
}

/**
 * The list itself, with no store behind it, so every state it can be in is
 * reachable from a test.
 */
@Composable
internal fun YourDevicesContent(
    shape: YourDevicesShape,
    items: List<YourDeviceListItem>,
    canAddDevice: Boolean,
    modifier: Modifier = Modifier,
    onAddDevice: () -> Unit,
    onRename: (YourDeviceListItem) -> Unit,
    onRemove: (YourDeviceListItem) -> Unit,
) {
    // The device that holds the signing role, named the way the person named it.
    // Both withheld states below point at it, because "use the other one" is
    // useless advice on a fleet of three.
    val approverName = items.firstOrNull { it.row.approves }
        ?.let { deviceDisplayName(it) }
        ?: stringResource(R.string.ui_this_phone)
    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
    ) {
        Text(
            stringResource(
                when (shape) {
                    YourDevicesShape.NEVER_LINKED -> R.string.ui_your_devices_only_this_one
                    YourDevicesShape.ONLY_THIS_DEVICE -> R.string.ui_your_devices_only_this_one
                    YourDevicesShape.SEVERAL -> R.string.ui_your_devices_intro
                },
            ),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(16.dp))
        val rows = items.map { it.row }
        for ((index, item) in items.withIndex()) {
            if (index > 0) HorizontalDivider()
            DeviceRow(
                item = item,
                // Why Remove is absent, said under the row it is absent from.
                // The reason was already worked out and simply never shown, so
                // a person looking for the button read a missing control as a
                // fault in the app rather than a rule about which phone to use.
                blocked = removeBlockedReason(rows, item.row),
                approverName = approverName,
                onRename = { onRename(item) },
                onRemove = { onRemove(item) },
            )
        }
        if (items.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            Text(
                stringResource(R.string.ui_your_devices_names_are_local),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Spacer(Modifier.height(24.dp))
        // Section 9.5's signature is the approving device's, so the ceremony can
        // only be finished there. Offering the button anywhere else would walk a
        // person through a code, a camera and six digits and fail at the last
        // step; withholding it and saying which phone to use costs one line.
        if (canAddDevice) {
            Button(onClick = onAddDevice, modifier = Modifier.fillMaxWidth()) {
                Text(stringResource(R.string.ui_add_a_device))
            }
            Text(
                stringResource(R.string.ui_add_a_device_summary),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )
        } else {
            Text(
                stringResource(R.string.ui_add_a_device_wrong_phone, approverName),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun DeviceRow(
    item: YourDeviceListItem,
    blocked: RemoveDeviceBlock?,
    approverName: String,
    onRename: () -> Unit,
    onRemove: () -> Unit,
) {
    val name = deviceDisplayName(item)
    val renameLabel = stringResource(R.string.ui_rename_device_named, name)
    val removeLabel = stringResource(R.string.ui_remove_device_named, name)
    Column(modifier = Modifier.fillMaxWidth().padding(vertical = 12.dp)) {
        Text(name, style = MaterialTheme.typography.titleMedium)
        if (item.row.approves) {
            Text(
                stringResource(R.string.ui_device_approves_new_devices),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        if (item.row.deviceIdHex.isNotEmpty()) {
            Text(
                stringResource(R.string.ui_device_code, shortDeviceCode(item.row.deviceIdHex)),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        item.firstSeenMs?.let { seen ->
            Text(
                stringResource(R.string.ui_device_seen_here_since, formatDay(seen)),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Row(modifier = Modifier.padding(top = 4.dp)) {
            TextButton(
                onClick = onRename,
                modifier = Modifier.semantics { contentDescription = renameLabel },
            ) { Text(stringResource(R.string.ui_rename)) }
            if (item.row.removable) {
                TextButton(
                    onClick = onRemove,
                    modifier = Modifier.semantics { contentDescription = removeLabel },
                ) { Text(stringResource(R.string.ui_remove_device)) }
            }
        }
        if (!item.row.removable && blocked != null) {
            Text(
                removeBlockText(blocked, approverName),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * Why Remove is missing from a row, in words that name the next thing to do.
 *
 * [RemoveDeviceBlock.NOT_THE_APPROVING_DEVICE] is the one that needs more than
 * a rule stated back at the person: it says which device can do it, and what to
 * do when that device is the one that is gone -- which is the situation they
 * are usually in when they came looking for Remove in the first place.
 */
@Composable
private fun removeBlockText(block: RemoveDeviceBlock, approverName: String): String =
    when (block) {
        RemoveDeviceBlock.NOT_THE_APPROVING_DEVICE ->
            stringResource(R.string.ui_remove_device_use_the_approver, approverName)
        else -> stringResource(removeBlockCopy(block))
    }

@Composable
private fun RenameDeviceDialog(
    item: YourDeviceListItem,
    onDismiss: () -> Unit,
    onSave: (String) -> Unit,
) {
    var text by remember(item.row.deviceIdHex) { mutableStateOf(item.name) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.ui_name_this_device)) },
        text = {
            Column {
                Text(stringResource(R.string.ui_your_devices_names_are_local))
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    singleLine = true,
                    label = { Text(stringResource(R.string.ui_device_name)) },
                    modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onSave(text) }) { Text(stringResource(R.string.ui_save)) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_cancel)) }
        },
    )
}

/**
 * §10.1 in family words: what the person loses, and what survives.
 *
 * Everything on this dialog is a consequence core will actually produce. It does
 * not promise the removed phone loses its Shore Pass mailbox, because §10.2's
 * relay-token rotation has no driver yet on either shell, and a confirmation
 * that overstates what removal does is worse than one that says less.
 */
@Composable
private fun RemoveDeviceDialog(
    item: YourDeviceListItem,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    val name = deviceDisplayName(item)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.ui_remove_this_device_question)) },
        text = {
            Column {
                Text(stringResource(R.string.ui_remove_device_what_happens, name))
                Text(
                    stringResource(R.string.ui_remove_device_what_survives),
                    modifier = Modifier.padding(top = 12.dp),
                )
                Text(
                    stringResource(R.string.ui_remove_device_cannot_undo),
                    modifier = Modifier.padding(top = 12.dp),
                )
            }
        },
        confirmButton = {
            // Error-coloured, matching iOS's destructive role: the two shells
            // must not disagree about how heavy this button looks.
            TextButton(onClick = onConfirm) {
                Text(
                    stringResource(R.string.ui_remove_device),
                    color = MaterialTheme.colorScheme.error,
                )
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_cancel)) }
        },
    )
}

@Composable
private fun RemovalOutcomeDialog(outcome: RemoveDeviceResult, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text(
                stringResource(
                    when (outcome) {
                        is RemoveDeviceResult.Removed -> R.string.ui_device_removed
                        is RemoveDeviceResult.Refused -> R.string.ui_device_not_removed
                    },
                ),
            )
        },
        text = {
            Column {
                Text(
                    when (outcome) {
                        is RemoveDeviceResult.Removed ->
                            if (outcome.siblingsToHandOffTo > 0) {
                                stringResource(R.string.ui_device_removed_others_catch_up)
                            } else {
                                stringResource(R.string.ui_device_removed_detail)
                            }
                        is RemoveDeviceResult.Refused -> stringResource(refusalCopy(outcome.reason))
                    },
                )
                // Section 10.1 re-seals the retained backlog to the survivors. A
                // record it could not re-seal is a message that will not arrive,
                // and counting it and then never saying so left the person with
                // a clean "Device removed" over a quiet loss.
                if (outcome is RemoveDeviceResult.Removed && outcome.unresealableRecords > 0u) {
                    Text(
                        stringResource(R.string.ui_device_removed_some_messages_lost),
                        modifier = Modifier.padding(top = 12.dp),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_done)) }
        },
    )
}

internal fun refusalCopy(reason: RemoveDeviceRefusal): Int = when (reason) {
    RemoveDeviceRefusal.NO_DEVICES -> R.string.ui_remove_device_no_devices
    RemoveDeviceRefusal.NOT_THE_APPROVING_DEVICE -> R.string.ui_remove_device_wrong_phone
    RemoveDeviceRefusal.INBOX_KEY_MISSING -> R.string.ui_remove_device_not_caught_up
    RemoveDeviceRefusal.NO_DEVICE_KEYS -> R.string.ui_remove_device_no_devices
    RemoveDeviceRefusal.EARLIER_REMOVAL_UNFINISHED -> R.string.ui_remove_device_earlier_one_unfinished
    RemoveDeviceRefusal.CORE_REFUSED -> R.string.ui_remove_device_failed
}

/** Why Remove is missing from a row, for a surface that wants to say so. */
internal fun removeBlockCopy(block: RemoveDeviceBlock): Int = when (block) {
    RemoveDeviceBlock.NOT_THE_APPROVING_DEVICE -> R.string.ui_remove_device_use_the_approver
    RemoveDeviceBlock.IS_THE_APPROVING_DEVICE -> R.string.ui_remove_device_is_the_approver
    RemoveDeviceBlock.LAST_DEVICE -> R.string.ui_remove_device_last_one
}

@Composable
private fun deviceDisplayName(item: YourDeviceListItem): String = when {
    item.name.isNotBlank() -> item.name
    item.row.isThisDevice -> stringResource(R.string.ui_this_phone)
    else -> stringResource(R.string.ui_device_numbered, item.row.position)
}

private fun formatDay(ms: Long): String =
    SimpleDateFormat("MMMM d, yyyy", Locale.getDefault()).format(Date(ms))
