package com.cruisemesh.app.identity.backup

import android.app.Activity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.text.KeyboardOptions
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** UI state shared by both flows: nothing running, working, done, or a typed error message. */
private sealed interface BackupUiState {
    object Idle : BackupUiState
    object Working : BackupUiState
    data class Error(val message: String) : BackupUiState
    object Done : BackupUiState
}

/**
 * Export flow (LOCAL_BACKUP_RESTORE.md §6): set a passphrase, then pick a
 * destination via the system file picker and write the encrypted `.cmbak`.
 * Self-contained — hosts its own SAF launcher and calls [BackupService]
 * directly so the navigation host only needs to add a route.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BackupExportScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var passphrase by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }
    var state by remember { mutableStateOf<BackupUiState>(BackupUiState.Idle) }

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
                    BackupService.buildBackup(context, passphrase.toCharArray())
                }
                withContext(Dispatchers.IO) { BackupService.writeBytes(context, uri, bytes) }
                BackupUiState.Done
            } catch (e: Exception) {
                BackupUiState.Error(e.message ?: "Backup failed")
            }
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Back up account") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back")
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

            PassphraseField(
                value = passphrase,
                onValueChange = { passphrase = it },
                label = "Backup passphrase",
            )
            PassphraseStrengthText(strength, passphrase.isEmpty())
            Spacer(Modifier.height(8.dp))
            PassphraseField(
                value = confirm,
                onValueChange = { confirm = it },
                label = "Confirm passphrase",
            )
            if (confirm.isNotEmpty() && !matches) {
                Text(
                    "Passphrases don't match",
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
                Text("Choose where to save")
            }

            StatusArea(
                state = state,
                workingLabel = "Encrypting and saving…",
                doneLabel = "Backup saved. Keep it and your passphrase somewhere safe.",
            )
        }
    }
}

/**
 * Restore flow (LOCAL_BACKUP_RESTORE.md §7): pick a `.cmbak`, enter the
 * passphrase, install the identity + message store, then relaunch so
 * everything is re-read from the restored state. Meant for the onboarding
 * "Restore from backup" branch (fresh install, no store open yet).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BackupRestoreScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var pickedName by remember { mutableStateOf<String?>(null) }
    var pickedBytes by remember { mutableStateOf<ByteArray?>(null) }
    var passphrase by remember { mutableStateOf("") }
    var state by remember { mutableStateOf<BackupUiState>(BackupUiState.Idle) }

    val openDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        state = BackupUiState.Idle
        scope.launch {
            try {
                pickedBytes = withContext(Dispatchers.IO) { BackupService.readBytes(context, uri) }
                pickedName = uri.lastPathSegment?.substringAfterLast('/')
            } catch (e: Exception) {
                state = BackupUiState.Error(e.message ?: "Could not read that file")
            }
        }
    }

    val canRestore = pickedBytes != null && passphrase.isNotEmpty() && state != BackupUiState.Working

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
                title = { Text("Restore from backup") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back")
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
                Text(if (pickedName == null) "Choose backup file" else "Choose a different file")
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
                onValueChange = { passphrase = it },
                label = "Backup passphrase",
            )

            Spacer(Modifier.height(24.dp))
            Button(
                onClick = {
                    val bytes = pickedBytes ?: return@Button
                    state = BackupUiState.Working
                    scope.launch {
                        try {
                            withContext(Dispatchers.IO) {
                                BackupService.restoreBackup(context, bytes, passphrase.toCharArray())
                            }
                            state = BackupUiState.Done
                            restart()
                        } catch (e: Exception) {
                            state = BackupUiState.Error(e.message ?: "Restore failed")
                        }
                    }
                },
                enabled = canRestore,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Restore")
            }

            StatusArea(
                state = state,
                workingLabel = "Decrypting and restoring…",
                doneLabel = "Restored. Restarting…",
            )
        }
    }
}

@Composable
private fun PassphraseField(value: String, onValueChange: (String) -> Unit, label: String) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun PassphraseStrengthText(strength: BackupPassphrase.Strength, empty: Boolean) {
    if (empty) {
        Text(
            "At least ${BackupPassphrase.MIN_LENGTH} characters.",
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
private fun StatusArea(state: BackupUiState, workingLabel: String, doneLabel: String) {
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
        BackupUiState.Done -> Text(
            doneLabel,
            color = MaterialTheme.colorScheme.primary,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 24.dp),
        )
        is BackupUiState.Error -> Text(
            state.message,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 24.dp),
        )
    }
}
