package com.cruisemesh.app.friending

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.ui.AvatarBadge
import kotlinx.coroutines.delay
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

private const val RECEIPT_TYPE_DELIVERED: UByte = 1u

data class FriendRequestDelivery(
    val reachedDirectly: Boolean,
    val lamport: ULong,
)

data class FriendAddedOutcome(
    val contact: Contact,
    val delivery: FriendRequestDelivery,
    val relayConfigured: Boolean,
)

data class FriendPreview(
    val contact: Contact,
    val keyChangeWarning: String? = null,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FriendConfirmationSheet(
    outcome: FriendAddedOutcome,
    ownUserId: ByteArray,
    store: MessageStore,
    onSayHi: () -> Unit,
    onAddAnother: (() -> Unit)?,
    onDone: () -> Unit,
) {
    var connected by remember(outcome.contact.userId, outcome.delivery) {
        // lamport == 0 is the receiving phone's direct-import event: that
        // phone has necessarily imported the peer already. On the scanning
        // phone, a BLE dispatch returning true only means locally queued; a
        // delivered receipt is what proves the other phone has the card.
        mutableStateOf(outcome.delivery.lamport == 0uL && outcome.delivery.reachedDirectly)
    }
    var avatar by remember(outcome.contact.userId) {
        mutableStateOf(store.contactAvatar(outcome.contact.userId))
    }

    LaunchedEffect(outcome.contact.userId, outcome.delivery.lamport) {
        while (!connected && outcome.delivery.lamport > 0uL) {
            connected = store.receiptThrough(
                outcome.contact.userId,
                ownUserId,
                RECEIPT_TYPE_DELIVERED,
            ) >= outcome.delivery.lamport
            if (!connected) delay(500)
        }
    }
    LaunchedEffect(outcome.contact.userId) {
        ChatEvents.changes.collect { changed ->
            if (changed.contentEquals(outcome.contact.userId)) {
                avatar = store.contactAvatar(outcome.contact.userId)
            }
        }
    }

    ModalBottomSheet(onDismissRequest = onDone) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                stringResource(if (connected) R.string.ui_connected else R.string.ui_friend_added),
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
            )
            FriendIdentityBlock(outcome.contact, avatar)

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (connected) {
                    Icon(Icons.Filled.CheckCircle, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                }
                Text(
                    when {
                        connected -> "You're connected. ${outcome.contact.name} has your card too."
                        outcome.relayConfigured ->
                            "Sending ${outcome.contact.name} your card through the relay so they can message you back."
                        else ->
                            "Your card will reach ${outcome.contact.name} next time your phones are near each other. Until then, only you can start the chat."
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            Button(onClick = onSayHi, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.ui_say_hi)) }
            if (onAddAnother != null) {
                TextButton(onClick = onAddAnother, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.ui_add_another)) }
            }
            TextButton(onClick = onDone, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.ui_done)) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FriendPreviewSheet(
    preview: FriendPreview,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(stringResource(R.string.ui_add_this_friend), style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
            FriendIdentityBlock(preview.contact, null)
            preview.keyChangeWarning?.let {
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }
            Button(onClick = onConfirm, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.ui_add_this_friend_60651604)) }
            TextButton(onClick = onDismiss, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.ui_cancel)) }
        }
    }
}

@Composable
private fun FriendIdentityBlock(contact: Contact, avatar: ByteArray?) {
    AvatarBadge(
        userId = contact.userId,
        name = contact.name,
        displayId = formatUserId(contact.userId),
        photoBytes = avatar,
        size = 72.dp,
    )
    Text(contact.name, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
    Text(
        fingerprintWords(contact.userId).joinToString(" "),
        style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
        textAlign = TextAlign.Center,
    )
    Text(stringResource(R.string.ui_ask_them_to_check_these_words_match_their),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
    )
}
