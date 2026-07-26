package com.cruisemesh.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.util.Log
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
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
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
import com.cruisemesh.app.mesh.relayCheckFailureRes
import com.cruisemesh.app.mesh.shouldRetryFirstRelayCheck
import com.cruisemesh.app.relay.RelayClient
import com.cruisemesh.app.relay.RelayConfig
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayHttpException
import com.cruisemesh.app.friending.encodeQrBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.cruisemesh_core.RelaySetup
import uniffi.cruisemesh_core.makeRelaySetupCard
import uniffi.cruisemesh_core.parseRelaySetupText
import uniffi.cruisemesh_core.relaySetupIsOfficial

private sealed class PassSetupState {
    object Idle : PassSetupState()
    object Testing : PassSetupState()
    object Checking : PassSetupState()
    object Saved : PassSetupState()
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
    var pendingUntrusted by remember { mutableStateOf<RelaySetup?>(null) }
    var setupState by remember { mutableStateOf<PassSetupState>(PassSetupState.Idle) }
    var showCustom by remember { mutableStateOf(false) }
    var showManualEntry by remember { mutableStateOf(false) }
    var unverifiedSetup by remember { mutableStateOf<RelaySetup?>(null) }
    var setupQrLink by remember { mutableStateOf<String?>(null) }
    var showRemoveConfirmation by remember { mutableStateOf(false) }
    var customUrl by remember { mutableStateOf(configured?.relayUrl.orEmpty()) }
    var customToken by remember { mutableStateOf(configured?.relayToken.orEmpty()) }

    fun testAndSave(setup: RelaySetup) {
        pending = null
        pendingUntrusted = null
        unverifiedSetup = null
        setupState = PassSetupState.Testing
        scope.launch {
            suspend fun checkRelay() = runCatching {
                withContext(Dispatchers.IO) {
                    RelayClient.syncPresence(
                        RelayConfig(setup.relayUrl, setup.relayToken),
                        announce = emptyList(),
                        query = emptyList(),
                    )
                }
            }
            var result = checkRelay()
            ensureActive()
            val firstError = result.exceptionOrNull()
            if (firstError != null && shouldRetryFirstRelayCheck(firstError)) {
                Log.i(
                    "CruisePassSetup",
                    "Retrying initial check after ${firstError.javaClass.simpleName}",
                )
                delay(750)
                result = checkRelay()
                ensureActive()
            }
            result.onSuccess {
                RelayConfigStore.save(context, setup.relayUrl, setup.relayToken)
                configured = RelayConfig(setup.relayUrl, setup.relayToken)
                showManualEntry = false
                MeshConnectivityStatus.setRelayHealth(RelayHealth.Ok(System.currentTimeMillis()))
                RelaySyncEvents.requestSync()
                pending = null
                setupState = PassSetupState.Saved
            }.onFailure { error ->
                if (error !is RelayHttpException) {
                    unverifiedSetup = setup
                }
                Log.w(
                    "CruisePassSetup",
                    "Check failed for ${relayHost(setup.relayUrl)}: ${error.javaClass.simpleName}",
                    error,
                )
                setupState = PassSetupState.Failed(setupFailureMessage(context, error))
            }
        }
    }

    fun startSetup(text: String) {
        pending = null
        pendingUntrusted = null
        runCatching { parseRelaySetupText(text) }
            .onSuccess { setup ->
                val current = configured
                when {
                    current != null &&
                        (current.relayUrl != setup.relayUrl || current.relayToken != setup.relayToken) ->
                        pending = setup
                    current == null && !relaySetupIsOfficial(setup.relayUrl) ->
                        pendingUntrusted = setup
                    else -> testAndSave(setup)
                }
            }
            .onFailure {
                val message = context.getString(R.string.ui_that_setup_card_is_incomplete)
                setupState = PassSetupState.Failed(message)
            }
    }

    fun pasteAndStart() {
        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        input = clipboard.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty()
        if (input.isBlank()) {
            setupState = PassSetupState.Failed(
                context.getString(R.string.ui_copy_the_setup_card_first),
            )
        } else {
            startSetup(input)
        }
    }

    LaunchedEffect(initialCard) {
        if (!initialCard.isNullOrBlank()) startSetup(initialCard)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(
                            if (initialCard.isNullOrBlank()) {
                                R.string.ui_cruise_pass
                            } else {
                                R.string.ui_setting_up_cruise_pass
                            },
                        ),
                    )
                },
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
            if (!initialCard.isNullOrBlank()) {
                when (val state = setupState) {
                    PassSetupState.Idle, PassSetupState.Testing -> {
                        Text(
                            stringResource(R.string.ui_checking_your_cruise_pass),
                            style = MaterialTheme.typography.headlineSmall,
                        )
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier.padding(top = 20.dp),
                        ) {
                            CircularProgressIndicator(modifier = Modifier.padding(end = 12.dp))
                            Text(stringResource(R.string.ui_this_only_takes_a_moment))
                        }
                    }
                    is PassSetupState.Saved -> {
                        CruisePassReadyHeading(
                            text = stringResource(R.string.ui_you_are_all_set),
                        )
                        Text(
                            stringResource(R.string.ui_cruise_pass_is_ready_on_this_phone),
                            modifier = Modifier.padding(top = 8.dp),
                        )
                        Button(
                            onClick = onBack,
                            modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
                        ) { Text(stringResource(R.string.ui_done)) }
                    }
                    is PassSetupState.Checking -> {
                        Text(
                            stringResource(R.string.ui_setup_saved),
                            style = MaterialTheme.typography.headlineSmall,
                        )
                        Text(
                            stringResource(R.string.ui_well_finish_checking_when_this_phone_is_online),
                            modifier = Modifier.padding(top = 8.dp),
                        )
                        Button(
                            onClick = onBack,
                            modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
                        ) { Text(stringResource(R.string.ui_done)) }
                    }
                    is PassSetupState.Failed -> {
                        Text(
                            stringResource(R.string.ui_cruise_pass_wasnt_set_up),
                            style = MaterialTheme.typography.headlineSmall,
                        )
                        Text(
                            state.message,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                        OutlinedButton(
                            onClick = { startSetup(initialCard) },
                            modifier = Modifier.fillMaxWidth().padding(top = 20.dp),
                        ) { Text(stringResource(R.string.ui_try_again)) }
                    }
                }
                unverifiedSetup?.let { setup ->
                    OutlinedButton(
                        onClick = {
                            RelayConfigStore.save(context, setup.relayUrl, setup.relayToken)
                            configured = RelayConfig(setup.relayUrl, setup.relayToken)
                            MeshConnectivityStatus.setRelayHealth(RelayHealth.Checking)
                            RelaySyncEvents.requestSync()
                            unverifiedSetup = null
                            setupState = PassSetupState.Checking
                        },
                        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                    ) { Text(stringResource(R.string.ui_save_and_check_later)) }
                }
                if (setupState is PassSetupState.Failed) {
                    TextButton(
                        onClick = onBack,
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text(stringResource(R.string.ui_not_now)) }
                }
            } else {
                // Deliberately health-only: a Saved state from minutes ago must
                // not keep the green check alive after the relay starts
                // rejecting the pass.
                val verified = relayHealth is RelayHealth.Ok
                if (configured != null && verified) {
                    CruisePassReadyHeading(
                        text = stringResource(R.string.ui_cruise_pass_is_set_up),
                    )
                } else {
                    Text(
                        stringResource(
                            if (configured == null) {
                                R.string.ui_set_up_your_cruise_pass
                            } else {
                                R.string.ui_cruise_pass_is_configured
                            },
                        ),
                        style = MaterialTheme.typography.headlineSmall,
                    )
                }
                if (configured == null) {
                    Text(
                        stringResource(R.string.ui_paste_your_setup_card_well_test_it),
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
                if (configured != null) {
                    Text(
                        stringResource(R.string.ui_status, passStatus(relayHealth, setupState)),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }

                if (configured == null || showManualEntry) {
                    Button(
                        onClick = ::pasteAndStart,
                        enabled = setupState != PassSetupState.Testing,
                        modifier = Modifier.fillMaxWidth().padding(top = 20.dp),
                    ) { Text(stringResource(R.string.ui_paste_and_set_up)) }
                    TextButton(
                        onClick = { showManualEntry = !showManualEntry },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            stringResource(
                                if (showManualEntry) {
                                    R.string.ui_hide_manual_entry
                                } else {
                                    R.string.ui_enter_setup_card_manually
                                },
                            ),
                        )
                    }
                    if (showManualEntry) {
                        OutlinedTextField(
                            value = input,
                            onValueChange = { input = it },
                            label = { Text(stringResource(R.string.ui_setup_card)) },
                            placeholder = { Text(stringResource(R.string.ui_cmrelay1)) },
                            minLines = 3,
                            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                        )
                        OutlinedButton(
                            onClick = { startSetup(input) },
                            enabled = input.isNotBlank() && setupState != PassSetupState.Testing,
                            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                        ) { Text(stringResource(R.string.ui_check_and_save)) }
                    }
                } else {
                    OutlinedButton(
                        onClick = {
                            setupState = PassSetupState.Idle
                            showManualEntry = true
                        },
                        modifier = Modifier.fillMaxWidth().padding(top = 16.dp),
                    ) { Text(stringResource(R.string.ui_use_a_different_cruise_pass)) }
                }

                when (val state = setupState) {
                    PassSetupState.Testing -> Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(top = 16.dp),
                    ) {
                        CircularProgressIndicator(modifier = Modifier.padding(end = 12.dp))
                        Text(stringResource(R.string.ui_checking_and_saving))
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
                            setupState = PassSetupState.Checking
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
                    Text(
                        stringResource(R.string.ui_share_only_with_your_familys_phones),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    TextButton(
                        onClick = { showRemoveConfirmation = true },
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text(stringResource(R.string.ui_remove_cruise_pass_setup)) }
                }

                Spacer(modifier = Modifier.height(20.dp))
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
                                setupState = PassSetupState.Failed(
                                    context.getString(R.string.ui_enter_a_complete_https_relay_url_and_token),
                                )
                            }
                        },
                        enabled = customUrl.isNotBlank() && customToken.isNotBlank(),
                        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                    ) { Text(stringResource(R.string.ui_test_and_save)) }
                }
            }
        }
    }

    pending?.let { setup ->
        AlertDialog(
            onDismissRequest = {
                pending = null
                if (!initialCard.isNullOrBlank()) onBack()
            },
            title = { Text(stringResource(R.string.ui_replace_cruise_pass)) },
            text = {
                Column {
                    val current = configured
                    if (current == null) {
                        Text(stringResource(R.string.ui_host_pass, relayHost(setup.relayUrl)))
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
                TextButton(onClick = { testAndSave(setup) }) {
                    Text(stringResource(R.string.ui_replace_and_test))
                }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        pending = null
                        if (!initialCard.isNullOrBlank()) onBack()
                    },
                ) { Text(stringResource(R.string.ui_keep_current_pass)) }
            },
        )
    }

    pendingUntrusted?.let { setup ->
        AlertDialog(
            onDismissRequest = {
                pendingUntrusted = null
                if (!initialCard.isNullOrBlank()) onBack()
            },
            title = { Text(stringResource(R.string.ui_set_up_this_relay)) },
            text = {
                Column {
                    Text(stringResource(R.string.ui_host_pass, relayHost(setup.relayUrl)))
                    Text(
                        stringResource(R.string.ui_this_card_is_not_for_the_official_service),
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { testAndSave(setup) }) {
                    Text(stringResource(R.string.ui_set_up_and_test))
                }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        pendingUntrusted = null
                        if (!initialCard.isNullOrBlank()) onBack()
                    },
                ) { Text(stringResource(R.string.ui_cancel)) }
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

private fun setupFailureMessage(
    context: Context,
    error: Throwable,
): String = context.getString(relayCheckFailureRes(error, hasValidatedInternet(context)))

private fun hasValidatedInternet(context: Context): Boolean {
    val manager = context.getSystemService(ConnectivityManager::class.java)
    val capabilities = manager.activeNetwork?.let(manager::getNetworkCapabilities) ?: return false
    return capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
        capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
}

@Composable
private fun CruisePassReadyHeading(text: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(
            Icons.Filled.CheckCircle,
            contentDescription = stringResource(R.string.ui_cruise_pass_ready),
            tint = LocalReachabilityPalette.current.nearby,
            modifier = Modifier.padding(end = 10.dp).size(28.dp),
        )
        Text(text, style = MaterialTheme.typography.headlineSmall)
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
