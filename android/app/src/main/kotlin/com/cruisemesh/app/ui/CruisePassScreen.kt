package com.cruisemesh.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
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
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.mesh.MeshConnectivityStatus
import com.cruisemesh.app.mesh.RelayHealth
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.friending.encodeQrBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.cruisemesh_core.RelaySetup
import uniffi.cruisemesh_core.makeRelaySetupCard
import uniffi.cruisemesh_core.parseRelaySetupText

private sealed class PassSetupState {
    object Idle : PassSetupState()
    object Testing : PassSetupState()
    data class Checking(val relayUrl: String) : PassSetupState()
    data class Saved(val relayUrl: String) : PassSetupState()
    data class Failed(val message: String) : PassSetupState()
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CruisePassScreen(initialCard: String?, onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val relayHealth by MeshConnectivityStatus.relay.collectAsState()
    var configured by remember { mutableStateOf(RelayConfigStore.load(context)) }
    var input by remember { mutableStateOf(initialCard.orEmpty()) }
    var pending by remember { mutableStateOf<RelaySetup?>(null) }
    var parseError by remember { mutableStateOf<String?>(null) }
    var setupState by remember { mutableStateOf<PassSetupState>(PassSetupState.Idle) }
    var showCustom by remember { mutableStateOf(false) }
    var unverifiedSetup by remember { mutableStateOf<RelaySetup?>(null) }
    var setupQrLink by remember { mutableStateOf<String?>(null) }
    var showRemoveConfirmation by remember { mutableStateOf(false) }
    var customUrl by remember { mutableStateOf(configured?.relayUrl.orEmpty()) }
    var customToken by remember { mutableStateOf(configured?.relayToken.orEmpty()) }

    fun review(text: String) {
        runCatching { parseRelaySetupText(text) }
            .onSuccess {
                pending = it
                parseError = null
            }
            .onFailure {
                pending = null
                parseError = "That setup card is incomplete or invalid. Copy the whole CMRELAY1 card and try again."
            }
    }

    fun testAndSave(setup: RelaySetup) {
        pending = null
        unverifiedSetup = null
        setupState = PassSetupState.Testing
        scope.launch {
            val result = runCatching {
                withContext(Dispatchers.IO) {
                    RelayClient.syncPresence(
                        RelayConfig(setup.relayUrl, setup.relayToken),
                        announce = emptyList(),
                        query = emptyList(),
                    )
                }
            }
            result.onSuccess {
                RelayConfigStore.save(context, setup.relayUrl, setup.relayToken)
                configured = RelayConfig(setup.relayUrl, setup.relayToken)
                MeshConnectivityStatus.setRelayHealth(RelayHealth.Ok(System.currentTimeMillis()))
                RelaySyncEvents.requestSync()
                pending = null
                setupState = PassSetupState.Saved(setup.relayUrl)
            }.onFailure { error ->
                if (error !is RelayHttpException) unverifiedSetup = setup
                setupState = PassSetupState.Failed(
                    when {
                        (error as? RelayHttpException)?.relayCode == "family_expired" ->
                            "This Cruise Pass has expired. Renew it, then open the new setup link."
                        (error as? RelayHttpException)?.relayCode == "family_suspended" ->
                            "This Cruise Pass is suspended. Contact support for help."
                        error is RelayHttpException ->
                            "The relay rejected this setup. Check the card or contact support."
                        else ->
                            "CruiseMesh could not reach the relay. Retry, or save the setup and let CruiseMesh check when this phone is online."
                    },
                )
            }
        }
    }

    LaunchedEffect(initialCard) {
        if (!initialCard.isNullOrBlank()) review(initialCard)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Cruise Pass") },
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
            when (val state = setupState) {
                is PassSetupState.Saved -> {
                    Text("Cruise Pass is ready", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        "This phone can now use ${relayHost(state.relayUrl)} whenever it has internet.",
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
                is PassSetupState.Checking -> {
                    Text("Cruise Pass setup saved", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        "CruiseMesh will verify ${relayHost(state.relayUrl)} when this phone is online. It will not show Ready until that check succeeds.",
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
                else -> {
                    Text(
                        if (configured == null) "Set up your Cruise Pass" else "Cruise Pass is configured",
                        style = MaterialTheme.typography.headlineSmall,
                    )
                    Text(
                        if (configured == null) {
                            "Open the setup link from your purchase email. If it did not open here, paste the relay card below."
                        } else {
                            "Saved for ${relayHost(configured!!.relayUrl)}. You can replace it with a new setup card at any time."
                        },
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
            if (configured != null) {
                Text(
                    "Status: ${passStatus(relayHealth, setupState)}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }

            OutlinedTextField(
                value = input,
                onValueChange = { input = it },
                label = { Text("Relay card") },
                placeholder = { Text("CMRELAY1:…") },
                minLines = 3,
                modifier = Modifier.fillMaxWidth().padding(top = 20.dp),
            )
            Row(modifier = Modifier.fillMaxWidth().padding(top = 8.dp)) {
                OutlinedButton(
                    onClick = {
                        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        input = clipboard.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty()
                        if (input.isNotBlank()) review(input)
                    },
                    modifier = Modifier.weight(1f),
                ) { Text("Paste card") }
                Spacer(modifier = Modifier.padding(4.dp))
                Button(
                    onClick = { review(input) },
                    enabled = input.isNotBlank(),
                    modifier = Modifier.weight(1f),
                ) { Text("Review") }
            }
            parseError?.let {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
            when (val state = setupState) {
                PassSetupState.Testing -> Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.padding(top = 16.dp),
                ) {
                    CircularProgressIndicator(modifier = Modifier.padding(end = 12.dp))
                    Text("Checking the relay before saving…")
                }
                is PassSetupState.Failed -> Text(
                    state.message,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(top = 12.dp),
                )
                else -> Unit
            }
            unverifiedSetup?.let { setup ->
                OutlinedButton(
                    onClick = {
                        RelayConfigStore.save(context, setup.relayUrl, setup.relayToken)
                        configured = RelayConfig(setup.relayUrl, setup.relayToken)
                        MeshConnectivityStatus.setRelayHealth(RelayHealth.Checking)
                        RelaySyncEvents.requestSync()
                        unverifiedSetup = null
                        setupState = PassSetupState.Checking(setup.relayUrl)
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text("Save and check later") }
            }

            if (configured != null || setupState is PassSetupState.Saved) {
                Spacer(modifier = Modifier.height(24.dp))
                OutlinedButton(
                    onClick = {
                        val current = configured ?: return@OutlinedButton
                        val card = makeRelaySetupCard(current.relayUrl, current.relayToken)
                        val link = "https://cruisemesh.app/r#$card"
                        context.startActivity(
                            Intent.createChooser(
                                Intent(Intent.ACTION_SEND).apply {
                                    type = "text/plain"
                                    putExtra(Intent.EXTRA_TEXT, link)
                                },
                                "Set up another phone",
                            ),
                        )
                    },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Set up another phone") }
                OutlinedButton(
                    onClick = {
                        val current = configured ?: return@OutlinedButton
                        val card = makeRelaySetupCard(current.relayUrl, current.relayToken)
                        setupQrLink = "https://cruisemesh.app/r#$card"
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text("Show setup QR") }
                TextButton(
                    onClick = { showRemoveConfirmation = true },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Remove Cruise Pass setup") }
                Text(
                    "Anyone with this link can use your household relay. Share it only with people in your group.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
                Text(
                    "Each family phone needs this setup. A configured phone with internet can help move the family's queued messages; Cruise Pass does not share that phone's internet connection.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
            }

            Spacer(modifier = Modifier.height(24.dp))
            Row(
                modifier = Modifier.fillMaxWidth().clickable { showCustom = !showCustom },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Custom relay", modifier = Modifier.weight(1f))
                Icon(
                    if (showCustom) Icons.Default.KeyboardArrowUp else Icons.Default.KeyboardArrowDown,
                    contentDescription = null,
                )
            }
            if (showCustom) {
                Text(
                    "For self-hosted relays and development. Most people should use the setup card above.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
                OutlinedTextField(
                    value = customUrl,
                    onValueChange = { customUrl = it },
                    label = { Text("Relay URL") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
                OutlinedTextField(
                    value = customToken,
                    onValueChange = { customToken = it },
                    label = { Text("Relay token") },
                    singleLine = true,
                    textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
                Button(
                    onClick = {
                        runCatching {
                            val card = makeRelaySetupCard(customUrl, customToken)
                            parseRelaySetupText(card)
                        }.onSuccess(::testAndSave).onFailure {
                            setupState = PassSetupState.Failed("Enter a complete HTTPS relay URL and token.")
                        }
                    },
                    enabled = customUrl.isNotBlank() && customToken.isNotBlank(),
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text("Test and save") }
            }
        }
    }

    pending?.let { setup ->
        AlertDialog(
            onDismissRequest = { pending = null },
            title = { Text("Use this Cruise Pass?") },
            text = {
                Column {
                    val current = configured
                    if (current == null) {
                        Text("Relay: ${relayHost(setup.relayUrl)}")
                    } else {
                        Text("Replace ${relayHost(current.relayUrl)} with ${relayHost(setup.relayUrl)}?")
                    }
                    Text(
                        "This will replace any relay currently saved on this phone. The household token stays hidden.",
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { testAndSave(setup) }) { Text("Test and use") }
            },
            dismissButton = {
                TextButton(onClick = { pending = null }) { Text("Cancel") }
            },
        )
    }

    setupQrLink?.let { link ->
        val qr = remember(link) { encodeQrBitmap(link) }
        AlertDialog(
            onDismissRequest = { setupQrLink = null },
            title = { Text("Set up another family phone") },
            text = {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Image(
                        bitmap = qr,
                        contentDescription = "Cruise Pass setup QR code",
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text(
                        "Scan this with the other phone. It configures internet delivery; it does not add a contact.",
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { setupQrLink = null }) { Text("Done") }
            },
        )
    }

    if (showRemoveConfirmation) {
        AlertDialog(
            onDismissRequest = { showRemoveConfirmation = false },
            title = { Text("Remove Cruise Pass setup?") },
            text = { Text("Queued internet delivery will stop until another Cruise Pass or custom relay is set up. Nearby delivery still works.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        RelayConfigStore.save(context, "", "")
                        configured = null
                        MeshConnectivityStatus.setRelayHealth(RelayHealth.NoConfig)
                        setupState = PassSetupState.Idle
                        input = ""
                        customUrl = ""
                        customToken = ""
                        showRemoveConfirmation = false
                    },
                ) { Text("Remove") }
            },
            dismissButton = {
                TextButton(onClick = { showRemoveConfirmation = false }) { Text("Cancel") }
            },
        )
    }
}

private fun relayHost(url: String): String =
    runCatching { Uri.parse(url).host }.getOrNull().orEmpty().ifBlank { url }

private fun passStatus(health: RelayHealth, setupState: PassSetupState): String {
    if (setupState is PassSetupState.Checking || setupState is PassSetupState.Testing) {
        return "Checking setup…"
    }
    return when (health) {
        RelayHealth.NoConfig -> "Checking setup…"
        RelayHealth.Checking -> "Checking setup…"
        RelayHealth.NoInternet -> "Phone is offline · setup is saved"
        is RelayHealth.Ok -> "Ready · checked ${passRelativeAge(health.lastSyncMs)}"
        is RelayHealth.Failing -> "Service unavailable · try again later"
        is RelayHealth.Expired -> "Pass expired · renewal required"
        is RelayHealth.Suspended -> "Pass suspended · contact support"
        is RelayHealth.TokenRejected -> "Setup card rejected"
    }
}

private fun passRelativeAge(timestampMs: Long): String {
    val minutes = ((System.currentTimeMillis() - timestampMs).coerceAtLeast(0L) / 60_000L)
    return when {
        minutes == 0L -> "just now"
        minutes < 60L -> "${minutes}m ago"
        minutes < 24L * 60L -> "${minutes / 60L}h ago"
        else -> "${minutes / (24L * 60L)}d ago"
    }
}
