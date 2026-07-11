package com.cruisemesh.app.ui

import android.content.res.Configuration
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ContactDetailsSheet(
    contact: Contact,
    onDeleteContact: () -> Unit,
    onDismiss: () -> Unit,
    connectivityText: String? = null,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        ContactDetailsSheetContent(
            contact = contact,
            onDeleteContact = onDeleteContact,
            connectivityText = connectivityText,
            modifier = Modifier.padding(bottom = 24.dp),
        )
    }
}

@Composable
fun ContactDetailsSheetContent(
    contact: Contact,
    onDeleteContact: () -> Unit,
    modifier: Modifier = Modifier,
    connectivityText: String? = null,
) {
    val displayId = formatUserId(contact.userId)
    val displayName = ChatListLogic.displayNameOrId(contact.name, displayId)
    val fingerprint = fingerprintWords(contact.userId)

    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        AvatarBadge(
            userId = contact.userId,
            name = contact.name,
            displayId = displayId,
            size = 72.dp,
        )
        Text(
            text = displayName,
            style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
            modifier = Modifier.padding(top = 16.dp),
        )
        Text(
            text = displayId,
            style = MaterialTheme.typography.titleMedium.copy(fontFamily = FontFamily.Monospace),
            modifier = Modifier.padding(top = 8.dp),
            textAlign = TextAlign.Center,
        )

        if (connectivityText != null) {
            OutlinedCard(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 24.dp),
                shape = RoundedCornerShape(24.dp),
            ) {
                Column(modifier = Modifier.padding(20.dp)) {
                    Text(
                        text = "Connectivity",
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        text = connectivityText,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
        }

        OutlinedCard(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 24.dp),
            shape = RoundedCornerShape(24.dp),
        ) {
            Column(modifier = Modifier.padding(20.dp)) {
                Text(
                    text = "Safety words",
                    style = MaterialTheme.typography.titleMedium,
                )
                Spacer(modifier = Modifier.height(12.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    fingerprint.forEach { word ->
                        OutlinedCard(modifier = Modifier.weight(1f)) {
                            Text(
                                text = word,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .padding(vertical = 12.dp),
                                textAlign = TextAlign.Center,
                                style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.Medium),
                            )
                        }
                    }
                }
                Text(
                    text = "Read these aloud to verify.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 12.dp),
                )
            }
        }

        Button(
            onClick = onDeleteContact,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 24.dp)
                .height(52.dp),
        ) {
            Text("Delete contact")
        }
    }
}

@Preview(showBackground = true, name = "Contact Sheet")
@Preview(
    showBackground = true,
    name = "Contact Sheet Dark",
    uiMode = Configuration.UI_MODE_NIGHT_YES,
)
@Composable
private fun ContactDetailsSheetPreview() {
    CruiseMeshTheme {
        ContactDetailsSheetContent(
            contact = Contact(
                userId = byteArrayOf(0x01, 0x02),
                name = "Maya",
                signPk = ByteArray(32),
                agreePk = ByteArray(32),
                relayUrl = null,
                relayToken = null,
            ),
            onDeleteContact = {},
        )
    }
}
