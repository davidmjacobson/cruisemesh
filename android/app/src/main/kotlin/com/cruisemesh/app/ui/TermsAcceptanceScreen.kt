package com.cruisemesh.app.ui

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import com.cruisemesh.app.identity.PRIVACY_POLICY_URL
import com.cruisemesh.app.identity.TERMS_OF_USE_URL

@Composable
fun TermsAcceptanceScreen(onAccept: () -> Unit) {
    val context = LocalContext.current
    var agreed by rememberSaveable { mutableStateOf(false) }

    fun open(url: String) {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    }

    Scaffold { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            verticalArrangement = Arrangement.Center,
        ) {
            Text(stringResource(R.string.ui_before_you_start), style = MaterialTheme.typography.headlineLarge)
            Text(
                stringResource(R.string.ui_terms_messaging_conduct),
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 16.dp),
            )
            Text(
                stringResource(R.string.ui_terms_encryption_reporting),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 12.dp),
            )
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 24.dp)
                    .clickable { agreed = !agreed },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Checkbox(checked = agreed, onCheckedChange = { agreed = it })
                Text(
                    stringResource(R.string.ui_terms_acceptance_confirmation),
                    modifier = Modifier.padding(start = 8.dp),
                )
            }
            Button(
                onClick = onAccept,
                enabled = agreed,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 20.dp),
            ) {
                Text(stringResource(R.string.ui_i_agree))
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.Center,
            ) {
                TextButton(onClick = { open(TERMS_OF_USE_URL) }) {
                    Text(stringResource(R.string.ui_terms_of_use))
                }
                TextButton(onClick = { open(PRIVACY_POLICY_URL) }) {
                    Text(stringResource(R.string.ui_privacy_policy))
                }
            }
        }
    }
}
