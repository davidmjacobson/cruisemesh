package com.cruisemesh.app.ui

import android.content.Intent
import androidx.annotation.StringRes
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
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.R
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.debug.FieldMetricsExport
import com.cruisemesh.app.mesh.DirectPath
import com.cruisemesh.app.mesh.MeshConnectivityStatus
import com.cruisemesh.app.mesh.MeshRuntimeStatus
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.coreContactDisplayName
import java.text.DateFormat
import java.util.Date

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectionDetailsScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val runtime by MeshRuntimeStatus.state.collectAsState()
    val directPaths by MeshConnectivityStatus.directPaths.collectAsState()
    val relay by MeshConnectivityStatus.relay.collectAsState()
    val relayConfigured = RelayConfigStore.load(context) != null
    var revision by remember { mutableStateOf(0) }
    var showClear by remember { mutableStateOf(false) }
    var showAllActivity by remember { mutableStateOf(false) }
    var diagnosticLogging by remember { mutableStateOf(DebugFileLog.isEnabled(context)) }
    var hasCapturedDiagnostics by remember { mutableStateOf(DebugFileLog.hasCapturedLogs(context)) }
    var supportMessage by remember { mutableStateOf<String?>(null) }
    val contacts = remember(revision) { store.listContacts() }
    val summaries = remember(revision) { store.peerConnectionSummaries() }
    val events = remember(revision) { store.peerConnectionEvents(null, 50u) }
    val names = remember(contacts) {
        contacts.associate { UserIdHex.encode(it.userId) to coreContactDisplayName(it) }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_connection_details)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
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
            DetailCard("Overview") {
                DetailLine("CruiseMesh", runtime.label)
                val bluetoothCount = directPaths.count {
                    it.key in names && it.value == DirectPath.BLUETOOTH
                }
                val localWifiCount = directPaths.count {
                    it.key in names && it.value == DirectPath.LOCAL_WIFI
                }
                DetailLine("Bluetooth", if (bluetoothCount == 0) "Listening" else "$bluetoothCount active")
                DetailLine("Local Wi-Fi", if (localWifiCount == 0) "No active links" else "$localWifiCount active")
                DetailLine("Cruise Pass", relayLabel(relay, relayConfigured))
                Text(
                    stringResource(R.string.ui_cruisemesh_chooses_the_best_available_path_automatically_a),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 10.dp),
                )
            }

            Spacer(modifier = Modifier.height(18.dp))
            DetailCard("People") {
                if (contacts.isEmpty()) {
                    Text(stringResource(R.string.ui_no_friends_added_yet))
                } else {
                    for (contact in contacts) {
                        val hex = UserIdHex.encode(contact.userId)
                        val pathRows = summaries.filter { it.userId.contentEquals(contact.userId) }
                        val latest = latestPeerStatus(pathRows)
                        val status = when {
                            directPaths[hex] == DirectPath.LOCAL_WIFI ->
                                stringResource(R.string.ui_connected_now_via_local_wifi)
                            directPaths[hex] == DirectPath.BLUETOOTH ->
                                stringResource(R.string.ui_connected_now_via_bluetooth)
                            latest == null -> stringResource(R.string.ui_no_connection_history_yet)
                            else -> {
                                val pathId = transportLabelId(latest.transport)
                                if (pathId == null) {
                                    stringResource(
                                        statusTextNoPathId(latest.evidence),
                                        formatTime(latest.atMs),
                                    )
                                } else {
                                    stringResource(
                                        statusTextId(latest.evidence),
                                        stringResource(pathId),
                                        formatTime(latest.atMs),
                                    )
                                }
                            }
                        }
                        Column(modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                            Text(coreContactDisplayName(contact))
                            Text(
                                status,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }

            Spacer(modifier = Modifier.height(18.dp))
            DetailCard("Recent activity") {
                if (events.isEmpty()) {
                    Text(stringResource(R.string.ui_connection_activity_will_appear_here_as_cruisemesh_reaches))
                } else {
                    val visibleEvents =
                        if (showAllActivity) events else events.take(RECENT_ACTIVITY_PREVIEW_COUNT)
                    val unnamedFriend = stringResource(R.string.ui_unnamed_friend)
                    visibleEvents.forEach { event ->
                        val who = names[UserIdHex.encode(event.userId)] ?: unnamedFriend
                        val pathId = transportLabelId(event.transport)
                        val line = if (pathId == null) {
                            stringResource(
                                activityTextNoPathId(peerEvidenceOf(event.kind)),
                                who,
                                formatTime(event.occurredAtMs),
                            )
                        } else {
                            stringResource(
                                activityTextId(peerEvidenceOf(event.kind)),
                                who,
                                stringResource(pathId),
                                formatTime(event.occurredAtMs),
                            )
                        }
                        Text(
                            line,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(vertical = 5.dp),
                        )
                    }
                    if (events.size > RECENT_ACTIVITY_PREVIEW_COUNT) {
                        TextButton(
                            onClick = { showAllActivity = !showAllActivity },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            // Resolved before the Text() call: the localization
                            // gate rejects a conditional inside Text(...) because
                            // it cannot see that both branches are localized.
                            val toggleLabel = if (showAllActivity) {
                                stringResource(R.string.ui_show_less)
                            } else {
                                stringResource(R.string.ui_show_recent_activity, events.size)
                            }
                            Text(toggleLabel)
                        }
                    }
                }
            }

            Spacer(modifier = Modifier.height(18.dp))
            DetailCard("Support") {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(stringResource(R.string.ui_diagnostic_logging))
                        Text(
                            stringResource(
                                if (DebugFileLog.isDebuggableBuild(context)) {
                                    R.string.ui_diagnostic_logging_always_on
                                } else {
                                    R.string.ui_diagnostic_logging_tester_desc
                                },
                            ),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    androidx.compose.material3.Switch(
                        checked = diagnosticLogging,
                        enabled = !DebugFileLog.isDebuggableBuild(context),
                        onCheckedChange = {
                            diagnosticLogging = it
                            DebugFileLog.setOptIn(context, it)
                            supportMessage = if (it) {
                                context.getString(R.string.ui_diagnostic_logging_enabled_message)
                            } else {
                                context.getString(R.string.ui_diagnostic_logging_disabled_message)
                            }
                        },
                    )
                }
                Button(
                    onClick = {
                        DebugFileLog.shareIntent(context)?.let {
                            context.startActivity(Intent.createChooser(it, "Share CruiseMesh diagnostics"))
                        } ?: run { supportMessage = "No diagnostics captured this session yet." }
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                ) { Text(stringResource(R.string.ui_share_diagnostics)) }
                OutlinedButton(
                    onClick = {
                        DebugFileLog.deleteCapturedLogs(context)
                        hasCapturedDiagnostics = false
                        supportMessage = context.getString(R.string.ui_diagnostics_deleted)
                    },
                    enabled = hasCapturedDiagnostics,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text(stringResource(R.string.ui_delete_captured_diagnostics)) }
                OutlinedButton(
                    onClick = {
                        FieldMetricsExport.shareIntent(context)?.let {
                            context.startActivity(Intent.createChooser(it, "Export CruiseMesh field metrics"))
                        } ?: run { supportMessage = "No field metrics captured yet." }
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text(stringResource(R.string.ui_export_field_metrics)) }
                OutlinedButton(
                    onClick = { showClear = true },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text(stringResource(R.string.ui_clear_connection_history)) }
                Text(
                    stringResource(R.string.ui_history_contains_only_friend_identity_path_type_event),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 8.dp),
                )
                Text(
                    stringResource(R.string.ui_field_metrics_contain_hashed_chat_tags_route_types),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
                supportMessage?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 6.dp),
                    )
                }
            }
        }
    }

    if (showClear) {
        AlertDialog(
            onDismissRequest = { showClear = false },
            title = { Text(stringResource(R.string.ui_clear_connection_history_confirm)) },
            text = { Text(stringResource(R.string.ui_this_removes_local_connection_events_and_per_person)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        store.clearPeerConnectionHistory()
                        revision += 1
                        showClear = false
                    },
                ) { Text(stringResource(R.string.ui_clear)) }
            },
            dismissButton = { TextButton(onClick = { showClear = false }) { Text(stringResource(R.string.ui_cancel)) } },
        )
    }
}

@Composable
private fun DetailCard(title: String, content: @Composable () -> Unit) {
    Text(
        title,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, bottom = 8.dp),
    )
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.46f),
        ),
    ) {
        Column(modifier = Modifier.fillMaxWidth().padding(16.dp)) { content() }
    }
}

@Composable
private fun DetailLine(label: String, value: String) {
    Row(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Text(label, modifier = Modifier.weight(1f))
        Text(value, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

private fun relayLabel(
    health: com.cruisemesh.app.mesh.RelayHealth,
    configured: Boolean,
): String {
    if (!configured) return "Not configured"
    return when (health) {
        com.cruisemesh.app.mesh.RelayHealth.NoConfig,
        com.cruisemesh.app.mesh.RelayHealth.Checking -> "Checking setup"
        com.cruisemesh.app.mesh.RelayHealth.NoInternet -> "Waiting for internet"
        is com.cruisemesh.app.mesh.RelayHealth.Ok -> "Connected"
        is com.cruisemesh.app.mesh.RelayHealth.Failing -> "Unreachable"
        is com.cruisemesh.app.mesh.RelayHealth.Expired -> "Pass expired"
        is com.cruisemesh.app.mesh.RelayHealth.Suspended -> "Pass suspended"
        is com.cruisemesh.app.mesh.RelayHealth.TokenRejected -> "Setup rejected"
        is com.cruisemesh.app.mesh.RelayHealth.QuotaFull -> "Storage full"
        is com.cruisemesh.app.mesh.RelayHealth.MessageTooLarge -> "Message too large"
        is com.cruisemesh.app.mesh.RelayHealth.RateLimited -> "Syncing slowed"
    }
}

@StringRes
private fun statusTextId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_peer_status_sent_you_a_message
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_peer_status_received_your_message
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_peer_status_seen_online
    PeerEvidence.CONNECTED -> R.string.ui_peer_status_last_connected
    PeerEvidence.DISCONNECTED -> R.string.ui_peer_status_last_disconnected
}

/** [statusTextId]'s wording for evidence whose path we never observed. */
@StringRes
private fun statusTextNoPathId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_peer_status_sent_you_a_message_no_path
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_peer_status_received_your_message_no_path
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_peer_status_seen_online_no_path
    PeerEvidence.CONNECTED -> R.string.ui_peer_status_last_connected_no_path
    PeerEvidence.DISCONNECTED -> R.string.ui_peer_status_last_disconnected_no_path
}

@StringRes
private fun activityTextId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_peer_activity_sent_you_a_message
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_peer_activity_received_your_message
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_peer_activity_was_reachable
    PeerEvidence.CONNECTED -> R.string.ui_peer_activity_connected
    PeerEvidence.DISCONNECTED -> R.string.ui_peer_activity_disconnected
}

/** [activityTextId]'s wording for evidence whose path we never observed. */
@StringRes
private fun activityTextNoPathId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_peer_activity_sent_you_a_message_no_path
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_peer_activity_received_your_message_no_path
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_peer_activity_was_reachable_no_path
    PeerEvidence.CONNECTED -> R.string.ui_peer_activity_connected_no_path
    PeerEvidence.DISCONNECTED -> R.string.ui_peer_activity_disconnected_no_path
}

/**
 * The copy naming this path, or null when there is no path to name.
 *
 * Null exactly when core says the path was not observed
 * (`core_peer_transport_is_observed`) -- pinned by
 * `ConnectionActivityLogicTest`, so the two cannot drift apart. A caller that
 * gets null must switch to the no-path wording rather than substituting a
 * plausible-looking radio; that substitution is the bug this screen was fixed
 * for.
 */
@StringRes
internal fun transportLabelId(transport: PeerConnectionTransport): Int? = when (transport) {
    PeerConnectionTransport.BLUETOOTH -> R.string.ui_path_bluetooth
    PeerConnectionTransport.LOCAL_WIFI -> R.string.ui_path_local_wifi
    PeerConnectionTransport.CRUISE_PASS -> R.string.ui_path_cruise_pass
    PeerConnectionTransport.CARRIED -> null
}

private fun formatTime(ms: Long): String =
    DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(ms))

private const val RECENT_ACTIVITY_PREVIEW_COUNT = 10
