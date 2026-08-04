package com.cruisemesh.app.friending

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
import uniffi.cruisemesh_core.FriendCardMatch
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.SharedFriendCard
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
    /**
     * How this card relates to contacts already saved, decided in core
     * (`friend_card_match`) so both shells reach the same verdict.
     */
    val match: FriendCardMatch = FriendCardMatch.New,
    /**
     * Non-null when this card arrived as a shared contact card. It rides the
     * mutual `kind=3` back so the shared person's phone can verify who
     * authorized the share (specs/share-contact.md).
     */
    val shared: SharedFriendCard? = null,
    /** Who shared it, for the "Shared by ..." line. Never a verified badge. */
    val sharedByName: String? = null,
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
    val isUpdate = preview.match is FriendCardMatch.AlreadySaved
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                stringResource(
                    if (isUpdate) R.string.ui_update_this_friend_question else R.string.ui_add_this_friend,
                ),
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
            )
            // "Shared by Mom" means Mom passed this card along -- not that Mom
            // vouches for who this person is. Say only the first thing.
            preview.sharedByName?.let { sharer ->
                Text(
                    stringResource(R.string.ui_shared_by, sharer),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            FriendIdentityBlock(preview.contact, null)
            if (preview.shared != null) {
                SafetyWordsRow(preview.contact.name, preview.contact.userId)
            }
            FriendMatchNote(preview)
            Button(onClick = onConfirm, modifier = Modifier.fillMaxWidth()) {
                Text(
                    stringResource(
                        if (isUpdate) R.string.ui_update_this_friend else R.string.ui_add_this_friend_60651604,
                    ),
                )
            }
            TextButton(onClick = onDismiss, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.ui_cancel)) }
        }
    }
}

/**
 * What this card means next to the contacts already saved. A card whose UserID
 * is already on file is the same person re-sharing (new relay details after a
 * Shore Pass, say) — never a key change, even when a *different* contact
 * happens to share their display name.
 */
@Composable
private fun FriendMatchNote(preview: FriendPreview) {
    when (val match = preview.match) {
        is FriendCardMatch.New -> Unit

        is FriendCardMatch.AlreadySaved -> Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.ui_you_already_have_this_friend_saved_as, match.savedName),
                style = MaterialTheme.typography.bodyMedium,
                textAlign = TextAlign.Center,
            )
            if (match.nameSharedWithOther) {
                // Two contacts show the same name, so the name alone cannot say
                // which one this is. The safety words can.
                Text(
                    stringResource(R.string.ui_another_friend_also_shows_as, match.savedName),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                SafetyWordsRow(match.savedName, preview.contact.userId)
                Text(
                    stringResource(R.string.ui_give_one_of_them_a_nickname),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
        }

        is FriendCardMatch.NameTaken -> Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                stringResource(R.string.ui_you_already_have_a_different_friend_named, match.otherName),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                textAlign = TextAlign.Center,
            )
            SafetyWordsRow(stringResource(R.string.ui_this_card), preview.contact.userId)
            SafetyWordsRow(match.otherName, match.otherUserId)
            Text(
                stringResource(R.string.ui_ask_them_to_read_their_safety_words_aloud),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}

/** One friend's safety words, labelled, so two of them read side by side. */
@Composable
private fun SafetyWordsRow(label: String, userId: ByteArray) {
    Column(modifier = Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            fingerprintWords(userId).joinToString(" "),
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = FontFamily.Monospace,
        )
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
    // Safety-word verification moved to the contact's details ("Verify
    // contact") to keep the first-run surface simple (T10).
}
