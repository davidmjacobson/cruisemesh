package com.cruisemesh.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
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
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.debug.FieldMetricsExport
import com.cruisemesh.app.friending.encodeQrBitmap
import com.cruisemesh.app.mesh.LanManualEndpoint
import com.cruisemesh.app.mesh.LanSweepDisplayState
import com.cruisemesh.app.mesh.LanTransportDiagnostics
import com.cruisemesh.app.mesh.MeshConnectivityStatus
import com.cruisemesh.app.mesh.RelayHealth
import com.cruisemesh.app.mesh.lanEndpointLink
import com.cruisemesh.app.mesh.parseLanManualEndpoint
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.mesh.InboundEngine
import com.cruisemesh.app.mesh.InboundEngineSettings
import com.cruisemesh.app.relay.RelayEngineSettings
import com.cruisemesh.app.relay.RelayPassEngine
import uniffi.cruisemesh_core.lanDefaultTcpPort
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun InternalToolsScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val initialRelay = remember { RelayConfigStore.load(context) }
    var relayUrl by remember { mutableStateOf(initialRelay?.relayUrl.orEmpty()) }
    var relayToken by remember { mutableStateOf(initialRelay?.relayToken.orEmpty()) }
    var friendLanAddress by remember { mutableStateOf("") }
    var showLanQrEndpoint by remember { mutableStateOf<LanManualEndpoint?>(null) }
    val lanStatus by LanTransportDiagnostics.state.collectAsState()
    val relayHealth by MeshConnectivityStatus.relay.collectAsState()

    fun saveAndBack() {
        RelayConfigStore.save(context, relayUrl, relayToken)
        onBack()
    }

    BackHandler(onBack = ::saveAndBack)

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_internal_tools)) },
                navigationIcon = {
                    IconButton(onClick = ::saveAndBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
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
                .padding(24.dp),
        ) {
            // On a release build these switches are only here because someone
            // did the seven-tap run. Say plainly what they do before they are
            // touched. Debuggable builds skip it -- a developer knows.
            if (internalToolsUnlockedOnRelease(context)) {
                Text(
                    stringResource(R.string.ui_internal_tools_release_warning),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.fillMaxWidth().padding(bottom = 20.dp),
                )
            }

            Text(stringResource(R.string.ui_relay), style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(
                value = relayUrl,
                onValueChange = { relayUrl = it },
                label = { Text(stringResource(R.string.ui_relay_url)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            )
            OutlinedTextField(
                value = relayToken,
                onValueChange = { relayToken = it },
                label = { Text(stringResource(R.string.ui_family_token)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            )
            Text(stringResource(R.string.ui_when_any_family_phone_has_internet_queued_messages),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            if (relayHealth is RelayHealth.TokenRejected) {
                val isOfficial = uniffi.cruisemesh_core.relaySetupIsOfficial(relayUrl)
                val textRes = if (isOfficial) R.string.ui_shore_pass_token_rejected else R.string.ui_relay_token_rejected
                Text(stringResource(textRes),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }

            Spacer(modifier = Modifier.height(28.dp))
            Text(stringResource(R.string.ui_local_wi_fi_field_tools), style = MaterialTheme.typography.titleMedium)
            Text(lanStatus.state, modifier = Modifier.padding(top = 8.dp))
            lanStatus.localEndpoint?.let { endpoint ->
                Text(
                    stringResource(R.string.ui_this_phone, endpoint),
                    style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                    modifier = Modifier.padding(top = 8.dp),
                )
                TextButton(onClick = {
                    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    clipboard.setPrimaryClip(
                        ClipData.newPlainText(context.getString(R.string.ui_cruisemesh_lan_address), endpoint),
                    )
                    Toast.makeText(context, R.string.ui_local_address_copied, Toast.LENGTH_SHORT).show()
                }) { Text(stringResource(R.string.ui_copy_this_phone_s_address)) }
                TextButton(onClick = {
                    showLanQrEndpoint = parseLanManualEndpoint(endpoint, lanDefaultTcpPort().toInt())
                }) { Text(stringResource(R.string.ui_show_address_qr)) }
            }
            if (lanStatus.activePeerNames.isNotEmpty()) {
                Text(
                    stringResource(R.string.ui_secure_link, lanStatus.activePeerNames.joinToString()),
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
            lanStatus.probeStatus?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.primary)
            }
            val sweepStatus = when (lanStatus.sweepDisplayState) {
                LanSweepDisplayState.NONE -> null
                LanSweepDisplayState.CHECKING ->
                    R.string.ui_checking_this_network to MaterialTheme.colorScheme.onSurfaceVariant
                LanSweepDisplayState.ISOLATION_SUSPECTED ->
                    R.string.ui_wifi_appears_to_block_phone_to_phone_traffic to MaterialTheme.colorScheme.error
                LanSweepDisplayState.BLOCKED_BY_POLICY ->
                    R.string.ui_local_wi_fi_probes_were_denied to MaterialTheme.colorScheme.error
            }
            sweepStatus?.let { (messageId, color) ->
                Text(
                    stringResource(messageId),
                    style = MaterialTheme.typography.bodySmall,
                    color = color,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
            Text(
                stringResource(R.string.ui_lan_frames, lanStatus.sentFrames, lanStatus.receivedFrames),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            lanStatus.lastPeerEndpoint?.let {
                Text(stringResource(R.string.ui_last_peer, it), style = MaterialTheme.typography.bodySmall)
            }
            lanStatus.lastError?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
            }
            OutlinedTextField(
                value = friendLanAddress,
                onValueChange = { friendLanAddress = it },
                label = { Text(stringResource(R.string.ui_friend_ip_address)) },
                placeholder = { Text(stringResource(R.string.ui_10_0_0_42_45892)) },
                supportingText = { Text(stringResource(R.string.ui_the_port_is_optional)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            )
            Button(
                onClick = {
                    LanTransportDiagnostics.requestManualConnection(
                        friendLanAddress,
                        lanDefaultTcpPort().toInt(),
                    )?.let { Toast.makeText(context, it, Toast.LENGTH_LONG).show() }
                },
                enabled = friendLanAddress.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(stringResource(R.string.ui_connect_securely)) }
            Text(stringResource(R.string.ui_manual_connection_requires_an_accepted_friend_and_cruisemesh),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            Button(
                onClick = {
                    LanTransportDiagnostics.requestConnectionTest()?.let {
                        Toast.makeText(context, it, Toast.LENGTH_LONG).show()
                    }
                },
                enabled = lanStatus.activePeerNames.isNotEmpty(),
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_test_encrypted_lan_link)) }
            Button(
                onClick = {
                    LanTransportDiagnostics.requestSubnetScan()?.let {
                        Toast.makeText(context, it, Toast.LENGTH_LONG).show()
                    }
                },
                enabled = lanStatus.scanTotal == null,
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_search_this_24_network)) }
            lanStatus.scanTotal?.let { total ->
                Text(
                    stringResource(R.string.ui_checked_addresses, lanStatus.scanProgress ?: 0, total),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
            Text(stringResource(R.string.ui_subnet_search_probes_only_tcp_45892_with_low),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            Text(stringResource(R.string.ui_keep_wifi_guidance),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )

            Spacer(modifier = Modifier.height(28.dp))
            Text(stringResource(R.string.ui_diagnostics), style = MaterialTheme.typography.titleMedium)
            if (!DebugFileLog.isDebuggableBuild(context)) {
                var diagnosticLogging by remember { mutableStateOf(DebugFileLog.isOptedIn(context)) }
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) {
                    Text(
                        stringResource(R.string.ui_diagnostic_logging),
                        modifier = Modifier.weight(1f),
                    )
                    Switch(
                        checked = diagnosticLogging,
                        onCheckedChange = {
                            diagnosticLogging = it
                            DebugFileLog.setOptIn(context, it)
                        },
                    )
                }
            }
            Text(stringResource(R.string.ui_diagnostic_logging_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The relay engine switch. It lives here, behind the same door as
            // the manual relay fields, because a closed-test build is
            // release-signed and cannot be reached with `run-as` -- so a flag
            // settable only from a unit test could never produce the on-device
            // evidence the migration needs before its default may move.
            var rebuiltRelayEngine by remember {
                mutableStateOf(RelayEngineSettings.passEngine(context) == RelayPassEngine.CORE)
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_rebuilt_internet_sync),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = rebuiltRelayEngine,
                    onCheckedChange = {
                        rebuiltRelayEngine = it
                        RelayEngineSettings.setPassEngine(
                            context,
                            if (it) RelayPassEngine.CORE else RelayPassEngine.LEGACY,
                        )
                    },
                )
            }
            Text(stringResource(R.string.ui_rebuilt_internet_sync_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The canary switch, which iOS has already. It defaults to on and
            // changes nothing the device sends or stores -- but a tester who
            // has to rule the canary out as a cause of something needs a way
            // to turn it off without a new build.
            var relayShadow by remember {
                mutableStateOf(RelayEngineSettings.shadowEnabled(context))
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_relay_migration_canary),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = relayShadow,
                    onCheckedChange = {
                        relayShadow = it
                        RelayEngineSettings.setShadowEnabled(context, it)
                    },
                )
            }
            Text(stringResource(R.string.ui_relay_migration_canary_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The receive engine switch, here for the same reason as the one
            // above: the evidence that has to be gathered before its default
            // may move can only come from a release-signed device.
            var rebuiltInbound by remember {
                mutableStateOf(InboundEngineSettings.inboundEngine(context) == InboundEngine.CORE)
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_rebuilt_message_handling),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = rebuiltInbound,
                    onCheckedChange = {
                        rebuiltInbound = it
                        InboundEngineSettings.setInboundEngine(
                            context,
                            if (it) InboundEngine.CORE else InboundEngine.LEGACY,
                        )
                    },
                )
            }
            Text(stringResource(R.string.ui_rebuilt_message_handling_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            Button(
                onClick = {
                    val intent = DebugFileLog.shareIntent(context)
                    if (intent != null) {
                        context.startActivity(Intent.createChooser(intent, "Share debug log"))
                    } else {
                        Toast.makeText(context, R.string.ui_no_log_captured_yet, Toast.LENGTH_SHORT).show()
                    }
                },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_share_debug_log)) }

            Text(stringResource(R.string.ui_export_field_metrics_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 12.dp),
            )
            OutlinedButton(
                onClick = {
                    val intent = FieldMetricsExport.shareIntent(context)
                    if (intent != null) {
                        context.startActivity(Intent.createChooser(intent, "Export field metrics"))
                    } else {
                        Toast.makeText(context, R.string.ui_no_field_metrics_yet, Toast.LENGTH_SHORT).show()
                    }
                },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_export_field_metrics)) }

            // The way back out, for anyone who would rather not hunt for the
            // version line again. Only offered where it can do anything: a
            // debuggable build shows this screen regardless of the flag.
            //
            // No confirmation message: this navigates straight back to
            // Settings, and a toast fired on the way would land on top of the
            // version row there -- the one place in the app a stray tap
            // matters. The entry being gone is the confirmation.
            if (internalToolsUnlockedOnRelease(context)) {
                OutlinedButton(
                    onClick = {
                        InternalToolsUnlockStore.setUnlocked(context, false)
                        saveAndBack()
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
                ) { Text(stringResource(R.string.ui_internal_tools_hide)) }
            }
        }
    }

    showLanQrEndpoint?.let { endpoint ->
        val qr = remember(endpoint) { encodeQrBitmap(lanEndpointLink(endpoint)) }
        AlertDialog(
            onDismissRequest = { showLanQrEndpoint = null },
            title = { Text(stringResource(R.string.ui_cruisemesh_lan_address)) },
            text = {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Image(
                        bitmap = qr,
                        contentDescription = stringResource(R.string.ui_cruisemesh_lan_address_qr_code),
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text(stringResource(R.string.ui_scan_with_the_other_phone_s_camera_to),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { showLanQrEndpoint = null }) { Text(stringResource(R.string.ui_done)) }
            },
        )
    }
}

@Preview(showBackground = true)
@Preview(showBackground = true, name = "Advanced Dark", uiMode = Configuration.UI_MODE_NIGHT_YES)
@Composable
private fun InternalToolsScreenPreview() {
    CruiseMeshTheme { InternalToolsScreen(onBack = {}) }
}
