package com.cruisemesh.app.friending

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import com.cruisemesh.app.ui.AvatarBadge
import uniffi.cruisemesh_core.PendingSharedRequest
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId

/**
 * One inbound shared-card request as the screen needs it: the stored row plus
 * the two things only the caller can resolve — who the sharer is by name, and
 * whether this is at least the second time they have asked.
 */
data class PendingSharedRequestRow(
    val request: PendingSharedRequest,
    val sharerName: String,
    val offerSuppression: Boolean,
)

/**
 * **Friends → Waiting to connect** (specs/share-contact.md).
 *
 * Reachable rather than transient on purpose: a request that is never answered
 * must not be the only place its sender can be seen, and somebody who exists
 * only in `pending_shared_requests` is invisible to a block list built from
 * contacts. There is deliberately no **Block** here — blocking stays where it
 * is, so a child tapping through a prompt cannot silently sever a relationship;
 * **Don't ask again** is the exit, and it appears from the second ask onward.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WaitingToConnectScreen(
    rows: List<PendingSharedRequestRow>,
    onConnect: (PendingSharedRequest) -> Unit,
    onNotNow: (PendingSharedRequest) -> Unit,
    onDontAskAgain: (PendingSharedRequest) -> Unit,
    onBack: () -> Unit,
) {
    var selected by remember { mutableStateOf<PendingSharedRequestRow?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_waiting_to_connect)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
            )
        },
    ) { innerPadding ->
        Column(modifier = Modifier.fillMaxSize().padding(innerPadding)) {
            if (rows.isEmpty()) {
                Text(
                    stringResource(R.string.ui_no_one_is_waiting_to_connect),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.fillMaxWidth().padding(24.dp),
                    textAlign = TextAlign.Center,
                )
            } else {
                LazyColumn(modifier = Modifier.fillMaxWidth()) {
                    items(rows) { row ->
                        val displayId = formatUserId(row.request.requesterUserId)
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { selected = row }
                                .padding(16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            AvatarBadge(
                                userId = row.request.requesterUserId,
                                name = row.request.name,
                                displayId = displayId,
                            )
                            Spacer(modifier = Modifier.width(16.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    stringResource(R.string.ui_x_wants_to_connect, row.request.name),
                                    style = MaterialTheme.typography.bodyLarge,
                                )
                                Text(
                                    stringResource(R.string.ui_shared_by, row.sharerName),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
            }
        }
    }

    selected?.let { row ->
        ModalBottomSheet(onDismissRequest = { selected = null }) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 24.dp)
                    .padding(bottom = 24.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_x_wants_to_connect, row.request.name),
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                    textAlign = TextAlign.Center,
                )
                Text(
                    stringResource(R.string.ui_shared_by, row.sharerName),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                AvatarBadge(
                    userId = row.request.requesterUserId,
                    name = row.request.name,
                    displayId = formatUserId(row.request.requesterUserId),
                    size = 72.dp,
                )
                Text(
                    row.request.name,
                    style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    formatUserId(row.request.requesterUserId),
                    style = MaterialTheme.typography.bodyMedium,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    fingerprintWords(row.request.requesterUserId).joinToString(" "),
                    style = MaterialTheme.typography.bodyMedium,
                    fontFamily = FontFamily.Monospace,
                    textAlign = TextAlign.Center,
                )
                Button(
                    onClick = {
                        selected = null
                        onConnect(row.request)
                    },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(stringResource(R.string.ui_connect))
                }
                TextButton(
                    onClick = {
                        selected = null
                        onNotNow(row.request)
                    },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(stringResource(R.string.ui_not_now))
                }
                if (row.offerSuppression) {
                    TextButton(
                        onClick = {
                            selected = null
                            onDontAskAgain(row.request)
                        },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(stringResource(R.string.ui_dont_ask_again))
                    }
                }
            }
        }
    }
}
