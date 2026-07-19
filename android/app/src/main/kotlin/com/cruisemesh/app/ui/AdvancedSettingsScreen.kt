package com.cruisemesh.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.widget.Toast
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
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
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.friending.encodeQrBitmap
import com.cruisemesh.app.mesh.LanManualEndpoint
import com.cruisemesh.app.mesh.LanSweepDisplayState
import com.cruisemesh.app.mesh.LanTransportDiagnostics
import com.cruisemesh.app.mesh.lanEndpointLink
import com.cruisemesh.app.mesh.parseLanManualEndpoint
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.lanDefaultTcpPort
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdvancedSettingsScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val initialRelay = remember { RelayConfigStore.load(context) }
    var relayUrl by remember { mutableStateOf(initialRelay?.relayUrl.orEmpty()) }
    var relayToken by remember { mutableStateOf(initialRelay?.relayToken.orEmpty()) }
    var friendLanAddress by remember { mutableStateOf("") }
    var showLanQrEndpoint by remember { mutableStateOf<LanManualEndpoint?>(null) }
    val lanStatus by LanTransportDiagnostics.state.collectAsState()

    fun saveAndBack() {
        RelayConfigStore.save(context, relayUrl, relayToken)
        onBack()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_advanced_settings)) },
                navigationIcon = {
                    IconButton(onClick = ::saveAndBack) {
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
                .padding(24.dp),
        ) {
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

            Spacer(modifier = Modifier.height(28.dp))
            Text(stringResource(R.string.ui_local_wi_fi_experimental), style = MaterialTheme.typography.titleMedium)
            Text(lanStatus.state, modifier = Modifier.padding(top = 8.dp))
            lanStatus.localEndpoint?.let { endpoint ->
                Text(
                    stringResource(R.string.ui_this_phone, endpoint),
                    style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                    modifier = Modifier.padding(top = 8.dp),
                )
                TextButton(onClick = {
                    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    clipboard.setPrimaryClip(ClipData.newPlainText("CruiseMesh LAN address", endpoint))
                    Toast.makeText(context, "Local address copied", Toast.LENGTH_SHORT).show()
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

            if (DebugFileLog.isEnabled(context)) {
                Spacer(modifier = Modifier.height(28.dp))
                Text(stringResource(R.string.ui_diagnostics), style = MaterialTheme.typography.titleMedium)
                Button(
                    onClick = {
                        val intent = DebugFileLog.shareIntent(context)
                        if (intent != null) {
                            context.startActivity(Intent.createChooser(intent, "Share debug log"))
                        } else {
                            Toast.makeText(context, "No log captured yet", Toast.LENGTH_SHORT).show()
                        }
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text(stringResource(R.string.ui_share_debug_log)) }
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
                        contentDescription = "CruiseMesh LAN address QR code",
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
private fun AdvancedSettingsScreenPreview() {
    CruiseMeshTheme { AdvancedSettingsScreen(onBack = {}) }
}
