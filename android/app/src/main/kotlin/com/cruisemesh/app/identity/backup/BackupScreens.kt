package com.cruisemesh.app.identity.backup

import android.app.Activity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.autofill.AutofillNode
import androidx.compose.ui.autofill.AutofillType
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalAutofill
import androidx.compose.ui.platform.LocalAutofillTree
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.text.KeyboardOptions
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.BackupContentOptions
import uniffi.cruisemesh_core.BackupInventory

/** UI state shared by both flows: nothing running, working, done, or a typed error message. */
private sealed interface BackupUiState {
    object Idle : BackupUiState
    object Working : BackupUiState
    data class Error(val text: BackupFailureText) : BackupUiState
    object Done : BackupUiState
}

/**
 * Resolve a failure to the sentence to display. Never read `message` off the
 * exception here: the core's typed exceptions carry an empty message, which is
 * non-null, so a `?:` fallback silently renders nothing at all.
 */
@Composable
private fun BackupFailureText.resolve(): String = when (this) {
    is BackupFailureText.Resource -> stringResource(resId)
    is BackupFailureText.Literal -> text
}

/**
 * Export flow: set a passphrase, then pick a
 * destination via the system file picker and write the encrypted `.cmbak`.
 * Self-contained — hosts its own SAF launcher and calls [BackupService]
 * directly so the navigation host only needs to add a route.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalComposeUiApi::class)
@Composable
fun BackupExportScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var passphrase by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }
    var state by remember { mutableStateOf<BackupUiState>(BackupUiState.Idle) }
    var inventory by remember { mutableStateOf<BackupInventory?>(null) }
    var includeHistory by remember { mutableStateOf(true) }
    var includeCourier by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        inventory = withContext(Dispatchers.IO) { BackupService.inventory(context) }
    }

    val strength = remember(passphrase) { BackupPassphrase.strength(passphrase.toCharArray()) }
    val acceptable = passphrase.length >= BackupPassphrase.MIN_LENGTH
    val matches = passphrase == confirm
    val canStart = acceptable && matches && state != BackupUiState.Working

    val createDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("application/octet-stream"),
    ) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        state = BackupUiState.Working
        scope.launch {
            state = try {
                val bytes = withContext(Dispatchers.IO) {
                    BackupService.buildBackup(
                        context,
                        passphrase.toCharArray(),
                        BackupContentOptions(includeHistory, includeCourier),
                    )
                }
                withContext(Dispatchers.IO) { BackupService.writeBytes(context, uri, bytes) }
                BackupUiState.Done
            } catch (e: Exception) {
                BackupUiState.Error(backupFailureText(e, R.string.ui_couldn_t_save_the_backup))
            }
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_back_up_account)) },
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
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            Spacer(Modifier.height(8.dp))
            WarningCard(
                "This file is your account. Anyone with the file and this " +
                    "passphrase can read your messages and impersonate you — and " +
                    "if you forget the passphrase, the backup can't be recovered. " +
                    "Store both carefully.",
            )
            Spacer(Modifier.height(16.dp))

            BackupChoice(
                checked = includeHistory,
                onCheckedChange = { includeHistory = it },
                title = "Include my message history",
                detail = inventory?.let {
                    "${it.messageCount} messages · ${formatBackupBytes(it.messageBytes)}; " +
                        "${it.pendingOwnDeliveryCount} pending deliveries from me"
                } ?: "Counting messages…",
            )
            BackupChoice(
                checked = includeCourier,
                onCheckedChange = { includeCourier = it },
                title = "Include pending deliveries for others",
                detail = inventory?.let {
                    "${it.pendingCourierDeliveryCount} encrypted messages · " +
                        formatBackupBytes(it.pendingCourierDeliveryBytes) +
                        ". They are unreadable on this phone."
                } ?: "Counting encrypted courier messages…",
            )
            Text(
                stringResource(R.string.ui_backup_identity_always_included),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            Spacer(Modifier.height(16.dp))

            PassphraseField(
                value = passphrase,
                onValueChange = { passphrase = it },
                label = "Backup passphrase",
                autofillType = AutofillType.NewPassword,
            )
            PassphraseStrengthText(strength, passphrase.isEmpty())
            Spacer(Modifier.height(8.dp))
            PassphraseField(
                value = confirm,
                onValueChange = { confirm = it },
                label = "Confirm passphrase",
                autofillType = AutofillType.NewPassword,
            )
            if (confirm.isNotEmpty() && !matches) {
                Text(stringResource(R.string.ui_passphrases_don_t_match),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }

            Spacer(Modifier.height(24.dp))
            Button(
                onClick = { createDocument.launch(BackupService.suggestedFileName()) },
                enabled = canStart,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(stringResource(R.string.ui_choose_where_to_save))
            }

            StatusArea(
                state = state,
                workingLabel = "Encrypting and saving…",
                doneLabel = null,
            )
        }
    }

    if (state == BackupUiState.Done) {
        BackupSavedDialog(onDismiss = { state = BackupUiState.Idle })
    }
}

@Composable
internal fun BackupSavedDialog(onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.ui_backup_saved)) },
        text = { Text(stringResource(R.string.ui_backup_saved_keep_it_and_your_passphrase)) },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.ui_done))
            }
        },
    )
}

/**
 * Restore flow: pick a `.cmbak`, enter the
 * passphrase, install the identity + message store, then relaunch so
 * everything is re-read from the restored state. Meant for the onboarding
 * "Restore from backup" branch (fresh install, no store open yet).
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalComposeUiApi::class)
@Composable
fun BackupRestoreScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var pickedName by remember { mutableStateOf<String?>(null) }
    var pickedBytes by remember { mutableStateOf<ByteArray?>(null) }
    var passphrase by remember { mutableStateOf("") }
    var state by remember { mutableStateOf<BackupUiState>(BackupUiState.Idle) }
    var preview by remember { mutableStateOf<BackupPreview?>(null) }
    var includeHistory by remember { mutableStateOf(true) }
    var includeCourier by remember { mutableStateOf(false) }

    val openDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        state = BackupUiState.Idle
        preview = null
        pickedBytes = null
        pickedName = null
        scope.launch {
            try {
                pickedBytes = withContext(Dispatchers.IO) { BackupService.readBytes(context, uri) }
                pickedName = withContext(Dispatchers.IO) { BackupService.displayName(context, uri) }
            } catch (e: Exception) {
                state = BackupUiState.Error(backupFailureText(e, R.string.ui_couldn_t_read_that_file))
            }
        }
    }

    val canReview = pickedBytes != null && passphrase.isNotEmpty() && state != BackupUiState.Working
    val canRestore = preview != null && state != BackupUiState.Working

    fun restart() {
        val intent = context.packageManager.getLaunchIntentForPackage(context.packageName)
        intent?.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK or android.content.Intent.FLAG_ACTIVITY_CLEAR_TASK)
        context.startActivity(intent)
        (context as? Activity)?.finish()
        Runtime.getRuntime().exit(0)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_restore_from_backup)) },
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
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            Spacer(Modifier.height(8.dp))
            WarningCard(
                "Restoring replaces this device's identity and message history " +
                    "with the backup's. Do this on a fresh install.",
            )
            Spacer(Modifier.height(16.dp))

            Button(
                onClick = { openDocument.launch(arrayOf("application/octet-stream", "*/*")) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(
                    stringResource(
                        if (pickedName == null) R.string.ui_choose_backup_file else R.string.ui_choose_different_file,
                    ),
                )
            }
            pickedName?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }

            Spacer(Modifier.height(16.dp))
            PassphraseField(
                value = passphrase,
                onValueChange = {
                    passphrase = it
                    preview = null
                },
                label = "Backup passphrase",
                autofillType = AutofillType.Password,
            )

            Spacer(Modifier.height(24.dp))
            if (preview == null) {
                Button(
                    onClick = {
                        val bytes = pickedBytes ?: return@Button
                        state = BackupUiState.Working
                        scope.launch {
                            state = try {
                                preview = withContext(Dispatchers.IO) {
                                    BackupService.previewBackup(context, bytes, passphrase.toCharArray())
                                }
                                BackupUiState.Idle
                            } catch (e: Exception) {
                                BackupUiState.Error(
                                    backupFailureText(e, R.string.ui_couldn_t_restore_that_backup),
                                )
                            }
                        }
                    },
                    enabled = canReview,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(stringResource(R.string.ui_review_backup))
                }
            }

            preview?.let { reviewed ->
                Text(
                    stringResource(
                        R.string.ui_backup_contains_contacts_and_groups,
                        reviewed.inventory.contactCount.toString(),
                        reviewed.inventory.groupCount.toString(),
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.Medium,
                )
                BackupChoice(
                    checked = includeHistory,
                    onCheckedChange = { includeHistory = it },
                    title = "Restore my message history",
                    detail = "${reviewed.inventory.messageCount} messages · " +
                        formatBackupBytes(reviewed.inventory.messageBytes),
                )
                BackupChoice(
                    checked = includeCourier,
                    onCheckedChange = { includeCourier = it },
                    title = "Restore pending deliveries for others",
                    detail = "${reviewed.inventory.pendingCourierDeliveryCount} encrypted messages · " +
                        formatBackupBytes(reviewed.inventory.pendingCourierDeliveryBytes),
                )
                Text(
                    stringResource(R.string.ui_backup_identity_always_restored),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(16.dp))
            }

            Button(
                onClick = {
                    // Take the bytes and drop the Compose-held copy so peak
                    // heap during decrypt/install is encrypted + sqlite, not
                    // encrypted × 2 + sqlite.
                    val bytes = pickedBytes ?: return@Button
                    pickedBytes = null
                    state = BackupUiState.Working
                    scope.launch {
                        try {
                            withContext(Dispatchers.IO) {
                                BackupService.restoreBackup(
                                    context,
                                    bytes,
                                    passphrase.toCharArray(),
                                    BackupContentOptions(includeHistory, includeCourier),
                                )
                            }
                            state = BackupUiState.Done
                            restart()
                        } catch (e: Exception) {
                            // Put the bytes back so the user can retry without
                            // re-picking the file after a failed restore.
                            pickedBytes = bytes
                            state = BackupUiState.Error(
                                backupFailureText(e, R.string.ui_couldn_t_restore_that_backup),
                            )
                        }
                    }
                },
                enabled = canRestore,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(stringResource(R.string.ui_restore))
            }

            StatusArea(
                state = state,
                workingLabel = "Decrypting and restoring…",
                doneLabel = "Restored. Restarting…",
            )
        }
    }
}

@OptIn(ExperimentalComposeUiApi::class)
@Composable
internal fun PassphraseField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    autofillType: AutofillType,
) {
    var visible by remember { mutableStateOf(false) }
    val autofill = LocalAutofill.current
    val autofillTree = LocalAutofillTree.current
    val latestOnValueChange by rememberUpdatedState(onValueChange)
    val autofillNode = remember(autofillType) {
        AutofillNode(
            autofillTypes = listOf(autofillType),
            onFill = { latestOnValueChange(it) },
        )
    }
    DisposableEffect(autofillTree, autofillNode) {
        autofillTree += autofillNode
        onDispose { autofillTree.children.remove(autofillNode.id) }
    }

    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        visualTransformation = if (visible) VisualTransformation.None else PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        trailingIcon = {
            TextButton(onClick = { visible = !visible }) {
                Text(
                    stringResource(
                        if (visible) R.string.ui_hide_passphrase else R.string.ui_show_passphrase,
                    ),
                )
            }
        },
        modifier = Modifier
            .fillMaxWidth()
            .onGloballyPositioned { coordinates ->
                autofillNode.boundingBox = coordinates.boundsInWindow()
            }
            .onFocusChanged { focusState ->
                if (focusState.isFocused) {
                    autofill?.requestAutofillForNode(autofillNode)
                } else {
                    autofill?.cancelAutofillForNode(autofillNode)
                }
            },
    )
}

@Composable
private fun PassphraseStrengthText(strength: BackupPassphrase.Strength, empty: Boolean) {
    if (empty) {
        Text(
            stringResource(R.string.ui_minimum_passphrase_length, BackupPassphrase.MIN_LENGTH),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(top = 4.dp),
        )
        return
    }
    val (label, color) = when (strength) {
        BackupPassphrase.Strength.TOO_SHORT ->
            "Too short — at least ${BackupPassphrase.MIN_LENGTH} characters" to MaterialTheme.colorScheme.error
        BackupPassphrase.Strength.WEAK -> "Weak — add length and variety" to MaterialTheme.colorScheme.error
        BackupPassphrase.Strength.FAIR -> "Fair" to MaterialTheme.colorScheme.onSurfaceVariant
        BackupPassphrase.Strength.STRONG -> "Strong" to MaterialTheme.colorScheme.primary
    }
    Text(label, color = color, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(top = 4.dp))
}

@Composable
private fun BackupChoice(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    title: String,
    detail: String,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .toggleable(
                value = checked,
                role = Role.Checkbox,
                onValueChange = onCheckedChange,
            )
            .padding(vertical = 4.dp),
        verticalAlignment = androidx.compose.ui.Alignment.Top,
    ) {
        Checkbox(
            checked = checked,
            onCheckedChange = null,
            modifier = Modifier.size(48.dp),
        )
        Column(modifier = Modifier.weight(1f).padding(top = 10.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private fun formatBackupBytes(bytes: ULong): String = when {
    bytes >= 1024uL * 1024uL -> "%.1f MB".format(bytes.toDouble() / (1024.0 * 1024.0))
    bytes >= 1024uL -> "%.1f KB".format(bytes.toDouble() / 1024.0)
    else -> "$bytes bytes"
}

@Composable
private fun WarningCard(text: String) {
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.errorContainer,
            contentColor = MaterialTheme.colorScheme.onErrorContainer,
        ),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(text, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(16.dp))
    }
}

@Composable
private fun StatusArea(state: BackupUiState, workingLabel: String, doneLabel: String?) {
    when (state) {
        BackupUiState.Idle -> {}
        BackupUiState.Working -> Column(
            modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
            horizontalAlignment = androidx.compose.ui.Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            CircularProgressIndicator()
            Text(workingLabel, style = MaterialTheme.typography.bodyMedium)
        }
        BackupUiState.Done -> doneLabel?.let {
            Text(
                it,
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(top = 24.dp),
            )
        }
        is BackupUiState.Error -> Text(
            state.text.resolve(),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 24.dp),
        )
    }
}
