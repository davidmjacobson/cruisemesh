package com.cruisemesh.app.ui

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HelpSupportScreen(
    onCruisePass: () -> Unit,
    onConnectionDetails: () -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_help_support)) },
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
            HelpItem(
                "Set up or fix Cruise Pass",
                "Paste the CMRELAY1 card from your purchase email, review it, then test and use it.",
                onCruisePass,
            )
            HelpItem(
                "Understand delivery",
                "See active paths, per-person connection history, recent activity, and diagnostic sharing.",
                onConnectionDetails,
            )
            HelpItem(
                "Setup link did not open",
                "Open Settings → Cruise Pass, paste the complete relay card, choose Review, then Test and use.",
                onCruisePass,
            )
            Text(
                stringResource(R.string.ui_never_post_a_cruise_pass_setup_card_or),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 16.dp),
            )
            Button(
                onClick = {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(SUPPORT_URL)))
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text(stringResource(R.string.ui_open_cruisemesh_support)) }
        }
    }
}

@Composable
private fun HelpItem(title: String, detail: String, onClick: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 14.dp),
    ) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        Text(
            detail,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 3.dp),
        )
    }
}
