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
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.friending.encodeQrBitmap
import com.cruisemesh.app.mesh.LanTransportDiagnostics
import com.cruisemesh.app.mesh.lanEndpointLink
import com.cruisemesh.app.mesh.parseLanManualEndpoint
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.lanDefaultTcpPort

/** Power-user controls kept one tap away from the main settings surface. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdvancedSettingsScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val savedRelay = remember { RelayConfigStore.load(context) }
    var relayUrl by remember { mutableStateOf(savedRelay?.relayUrl.orEmpty()) }
    var relayToken by remember { mutableStateOf(savedRelay?.relayToken.orEmpty()) }
    var friendLanAddress by remember { mutableStateOf("") }
    var showLanQrEndpoint by remember {
        mutableStateOf<com.cruisemesh.app.mesh.LanManualEndpoint?>(null)
    }
    val lanStatus by LanTransportDiagnostics.state.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Advanced") },
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
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            ProfileSection(title = "Relay") {
                Text(
                    "Optional family mailbox used when phones cannot reach each other directly.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
                OutlinedTextField(
                    value = relayUrl,
                    onValueChange = {
                        relayUrl = it
                        RelayConfigStore.save(context, relayUrl = it, relayToken = relayToken)
                    },
                    label = { Text("Relay URL") },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 12.dp),
                )
                OutlinedTextField(
                    value = relayToken,
                    onValueChange = {
                        relayToken = it
                        RelayConfigStore.save(context, relayUrl = relayUrl, relayToken = it)
                    },
                    label = { Text("Family token") },
                    visualTransformation = PasswordVisualTransformation(),
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 8.dp),
                )
            }

            Spacer(modifier = Modifier.height(16.dp))

            ProfileSection(title = "Local Wi-Fi tools") {
                Text(lanStatus.state, modifier = Modifier.padding(top = 4.dp))
                lanStatus.localEndpoint?.let { endpoint ->
                    Text(
                        "This phone: $endpoint",
                        style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    TextButton(
                        onClick = {
                            val clipboard =
                                context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                            clipboard.setPrimaryClip(
                                ClipData.newPlainText("CruiseMesh LAN address", endpoint),
                            )
                            Toast.makeText(context, "Local address copied", Toast.LENGTH_SHORT).show()
                        },
                    ) {
                        Text("Copy this phone's address")
                    }
                    TextButton(
                        onClick = {
                            showLanQrEndpoint = parseLanManualEndpoint(
                                endpoint,
                                lanDefaultTcpPort().toInt(),
                            )
                        },
                    ) {
                        Text("Show address QR")
                    }
                }
                if (lanStatus.activePeerNames.isNotEmpty()) {
                    Text(
                        "Secure link: ${lanStatus.activePeerNames.joinToString()}",
                        color = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                    Button(
                        onClick = {
                            val error = LanTransportDiagnostics.requestConnectionTest()
                            if (error != null) {
                                Toast.makeText(context, error, Toast.LENGTH_LONG).show()
                            }
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(top = 8.dp),
                    ) {
                        Text("Test encrypted LAN link")
                    }
                }
                lanStatus.probeStatus?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
                if (lanStatus.sentFrames > 0 || lanStatus.receivedFrames > 0) {
                    Text(
                        "LAN frames: ${lanStatus.sentFrames} sent · ${lanStatus.receivedFrames} received",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
                lanStatus.lastPeerEndpoint?.let {
                    Text(
                        "Last peer: $it",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
                lanStatus.lastError?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
                OutlinedTextField(
                    value = friendLanAddress,
                    onValueChange = { friendLanAddress = it },
                    label = { Text("Friend IP address") },
                    placeholder = { Text("10.0.0.42:45892") },
                    supportingText = { Text("The port is optional.") },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 8.dp),
                )
                Button(
                    onClick = {
                        val error = LanTransportDiagnostics.requestManualConnection(
                            friendLanAddress,
                            lanDefaultTcpPort().toInt(),
                        )
                        if (error != null) {
                            Toast.makeText(context, error, Toast.LENGTH_LONG).show()
                        }
                    },
                    enabled = friendLanAddress.isNotBlank(),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Connect securely")
                }
                Text(
                    "Manual connection only opens a TCP path. The other phone must already be an accepted friend and pass CruiseMesh's encrypted identity check.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
                Button(
                    onClick = {
                        val error = LanTransportDiagnostics.requestSubnetScan()
                        if (error != null) {
                            Toast.makeText(context, error, Toast.LENGTH_LONG).show()
                        }
                    },
                    enabled = lanStatus.scanTotal == null,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 8.dp),
                ) {
                    Text("Search this /24 network")
                }
                if (lanStatus.scanTotal != null) {
                    Text(
                        "Checked ${lanStatus.scanProgress ?: 0} of ${lanStatus.scanTotal} addresses",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
                Text(
                    "Subnet search is user-triggered, probes only TCP 45892 with low concurrency, and never expands to a /16.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }

            if (DebugFileLog.isEnabled(context)) {
                Spacer(modifier = Modifier.height(16.dp))
                ProfileSection(title = "Diagnostics") {
                    Text(
                        "On-device log capture is on. Share it to send diagnostics.",
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
                                Toast.makeText(context, "No log captured yet", Toast.LENGTH_SHORT).show()
                            }
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(top = 8.dp),
                    ) {
                        Text("Share debug log")
                    }
                }
            }
        }
    }

    showLanQrEndpoint?.let { endpoint ->
        val link = remember(endpoint) { lanEndpointLink(endpoint) }
        val qr = remember(link) { encodeQrBitmap(link) }
        AlertDialog(
            onDismissRequest = { showLanQrEndpoint = null },
            title = { Text("CruiseMesh LAN address") },
            text = {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Image(
                        bitmap = qr,
                        contentDescription = "CruiseMesh LAN address QR code",
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text(
                        "Scan with the other phone's camera to open CruiseMesh and connect securely.",
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { showLanQrEndpoint = null }) {
                    Text("Done")
                }
            },
        )
    }
}

@Preview(showBackground = true)
@Preview(
    showBackground = true,
    name = "Advanced dark",
    uiMode = Configuration.UI_MODE_NIGHT_YES,
)
@Composable
private fun AdvancedSettingsScreenPreview() {
    CruiseMeshTheme {
        AdvancedSettingsScreen(onBack = {})
    }
}
