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
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
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
                title = { Text(stringResource(R.string.ui_cruise_pass)) },
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
                    Text(stringResource(R.string.ui_cruise_pass_is_ready), style = MaterialTheme.typography.headlineSmall)
                    Text(
                        stringResource(
                            R.string.ui_this_phone_can_now_use_whenever_it_has,
                            relayHost(state.relayUrl),
                        ),
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
                is PassSetupState.Checking -> {
                    Text(stringResource(R.string.ui_cruise_pass_setup_saved), style = MaterialTheme.typography.headlineSmall)
                    Text(
                        stringResource(
                            R.string.ui_cruisemesh_will_verify_when_this_phone_is_online,
                            relayHost(state.relayUrl),
                        ),
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
                else -> {
                    Text(
                        stringResource(
                            if (configured == null) R.string.ui_set_up_your_cruise_pass
                            else R.string.ui_cruise_pass_is_configured,
                        ),
                        style = MaterialTheme.typography.headlineSmall,
                    )
                    val configuredSummary = if (configured == null) {
                        stringResource(R.string.ui_open_the_setup_link_from_your_purchase_email)
                    } else {
                        stringResource(R.string.ui_saved_for, relayHost(configured!!.relayUrl))
                    }
                    Text(
                        configuredSummary,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
            if (configured != null) {
                Text(
                    stringResource(R.string.ui_status, passStatus(relayHealth, setupState)),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }

            OutlinedTextField(
                value = input,
                onValueChange = { input = it },
                label = { Text(stringResource(R.string.ui_relay_card)) },
                placeholder = { Text(stringResource(R.string.ui_cmrelay1)) },
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
                ) { Text(stringResource(R.string.ui_paste_card)) }
                Spacer(modifier = Modifier.padding(4.dp))
                Button(
                    onClick = { review(input) },
                    enabled = input.isNotBlank(),
                    modifier = Modifier.weight(1f),
                ) { Text(stringResource(R.string.ui_review)) }
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
                    Text(stringResource(R.string.ui_checking_the_relay_before_saving))
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
                ) { Text(stringResource(R.string.ui_save_and_check_later)) }
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
                ) { Text(stringResource(R.string.ui_set_up_another_phone)) }
                OutlinedButton(
                    onClick = {
                        val current = configured ?: return@OutlinedButton
                        val card = makeRelaySetupCard(current.relayUrl, current.relayToken)
                        setupQrLink = "https://cruisemesh.app/r#$card"
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) { Text(stringResource(R.string.ui_show_setup_qr)) }
                TextButton(
                    onClick = { showRemoveConfirmation = true },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(stringResource(R.string.ui_remove_cruise_pass_setup)) }
                Text(
                    stringResource(R.string.ui_anyone_with_this_link_can_use_your_household),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
                Text(
                    stringResource(R.string.ui_each_family_phone_needs_this_setup_a_configured),
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
                Text(stringResource(R.string.ui_custom_relay), modifier = Modifier.weight(1f))
                Icon(
                    if (showCustom) Icons.Default.KeyboardArrowUp else Icons.Default.KeyboardArrowDown,
                    contentDescription = null,
                )
            }
            if (showCustom) {
                Text(
                    stringResource(R.string.ui_for_self_hosted_relays_and_development_most_people),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 6.dp),
                )
                OutlinedTextField(
                    value = customUrl,
                    onValueChange = { customUrl = it },
                    label = { Text(stringResource(R.string.ui_relay_url)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
                OutlinedTextField(
                    value = customToken,
                    onValueChange = { customToken = it },
                    label = { Text(stringResource(R.string.ui_relay_token)) },
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
                ) { Text(stringResource(R.string.ui_test_and_save)) }
            }
        }
    }

    pending?.let { setup ->
        AlertDialog(
            onDismissRequest = { pending = null },
            title = { Text(stringResource(R.string.ui_use_this_cruise_pass)) },
            text = {
                Column {
                    val current = configured
                    if (current == null) {
                        Text(stringResource(R.string.ui_relay_pass, relayHost(setup.relayUrl)))
                    } else {
                        Text(
                            stringResource(
                                R.string.ui_replace_with,
                                relayHost(current.relayUrl),
                                relayHost(setup.relayUrl),
                            ),
                        )
                    }
                    Text(
                        stringResource(R.string.ui_this_will_replace_any_relay_currently_saved_on),
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { testAndSave(setup) }) { Text(stringResource(R.string.ui_test_and_use)) }
            },
            dismissButton = {
                TextButton(onClick = { pending = null }) { Text(stringResource(R.string.ui_cancel)) }
            },
        )
    }

    setupQrLink?.let { link ->
        val qr = remember(link) { encodeQrBitmap(link) }
        AlertDialog(
            onDismissRequest = { setupQrLink = null },
            title = { Text(stringResource(R.string.ui_set_up_another_family_phone)) },
            text = {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Image(
                        bitmap = qr,
                        contentDescription = "Cruise Pass setup QR code",
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text(
                        stringResource(R.string.ui_scan_this_with_the_other_phone_it_configures),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { setupQrLink = null }) { Text(stringResource(R.string.ui_done)) }
            },
        )
    }

    if (showRemoveConfirmation) {
        AlertDialog(
            onDismissRequest = { showRemoveConfirmation = false },
            title = { Text(stringResource(R.string.ui_remove_cruise_pass_setup_confirm)) },
            text = { Text(stringResource(R.string.ui_queued_internet_delivery_will_stop_until_another_cruise)) },
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
                ) { Text(stringResource(R.string.ui_remove)) }
            },
            dismissButton = {
                TextButton(onClick = { showRemoveConfirmation = false }) { Text(stringResource(R.string.ui_cancel)) }
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
