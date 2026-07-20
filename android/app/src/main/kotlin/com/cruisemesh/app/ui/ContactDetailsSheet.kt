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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.Switch
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ContactDetailsSheet(
    contact: Contact,
    avatarBytes: ByteArray? = null,
    onDeleteContact: () -> Unit,
    onDismiss: () -> Unit,
    connectivityText: String? = null,
    isMuted: Boolean = false,
    onMutedChange: (Boolean) -> Unit = {},
    onSetNickname: (String?) -> Unit = {},
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        ContactDetailsSheetContent(
            contact = contact,
            avatarBytes = avatarBytes,
            onDeleteContact = onDeleteContact,
            connectivityText = connectivityText,
            isMuted = isMuted,
            onMutedChange = onMutedChange,
            onSetNickname = onSetNickname,
            modifier = Modifier.padding(bottom = 24.dp),
        )
    }
}

@Composable
fun ContactDetailsSheetContent(
    contact: Contact,
    avatarBytes: ByteArray? = null,
    onDeleteContact: () -> Unit,
    modifier: Modifier = Modifier,
    connectivityText: String? = null,
    isMuted: Boolean = false,
    onMutedChange: (Boolean) -> Unit = {},
    onSetNickname: (String?) -> Unit = {},
) {
    val displayId = formatUserId(contact.userId)
    val displayName = ChatListLogic.displayNameOrId(coreContactDisplayName(contact), displayId)
    val hasNickname = !contact.nickname.isNullOrBlank()
    val fingerprint = fingerprintWords(contact.userId)
    var editingNickname by remember(contact.userId) { mutableStateOf(false) }

    if (editingNickname) {
        NicknameEditDialog(
            initial = contact.nickname.orEmpty(),
            cardName = contact.name,
            onDismiss = { editingNickname = false },
            onSave = { value ->
                onSetNickname(value)
                editingNickname = false
            },
        )
    }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        AvatarBadge(
            userId = contact.userId,
            name = displayName,
            displayId = displayId,
            size = 72.dp,
            photoBytes = avatarBytes,
        )
        Text(
            text = displayName,
            style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
            modifier = Modifier.padding(top = 16.dp),
        )
        if (hasNickname) {
            Text(
                text = stringResource(R.string.ui_also_known_as, contact.name),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
                textAlign = TextAlign.Center,
            )
        }
        Text(
            text = displayId,
            style = MaterialTheme.typography.titleMedium.copy(fontFamily = FontFamily.Monospace),
            modifier = Modifier.padding(top = 8.dp),
            textAlign = TextAlign.Center,
        )
        OutlinedButton(
            onClick = { editingNickname = true },
            modifier = Modifier.padding(top = 12.dp),
        ) {
            Text(
                stringResource(
                    if (hasNickname) R.string.ui_edit_nickname else R.string.ui_add_a_nickname,
                ),
            )
        }

        if (connectivityText != null) {
            OutlinedCard(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 24.dp),
                shape = RoundedCornerShape(24.dp),
            ) {
                Column(modifier = Modifier.padding(20.dp)) {
                    Text(text = stringResource(R.string.ui_connectivity),
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
                Text(text = stringResource(R.string.ui_safety_words),
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
                Text(text = stringResource(R.string.ui_read_these_aloud_to_verify),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 12.dp),
                )
            }
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(top = 16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(stringResource(R.string.ui_mute_notifications), modifier = Modifier.weight(1f))
            Switch(checked = isMuted, onCheckedChange = onMutedChange)
        }

        Button(
            onClick = onDeleteContact,
            colors = ButtonDefaults.buttonColors(
                containerColor = MaterialTheme.colorScheme.error,
                contentColor = MaterialTheme.colorScheme.onError,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 24.dp)
                .height(52.dp),
        ) {
            Text(stringResource(R.string.ui_delete_contact))
        }
    }
}

@Composable
private fun NicknameEditDialog(
    initial: String,
    cardName: String,
    onDismiss: () -> Unit,
    onSave: (String?) -> Unit,
) {
    var text by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.ui_nickname)) },
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    singleLine = true,
                    label = { Text(stringResource(R.string.ui_nickname)) },
                    placeholder = { Text(cardName) },
                    modifier = Modifier.fillMaxWidth(),
                )
                Text(
                    text = stringResource(R.string.ui_nickname_is_only_shown_to_you),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onSave(text.trim().ifBlank { null }) }) {
                Text(stringResource(R.string.ui_save))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.ui_cancel))
            }
        },
    )
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
