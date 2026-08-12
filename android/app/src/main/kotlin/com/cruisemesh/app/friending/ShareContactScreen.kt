package com.cruisemesh.app.friending

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.FriendCard
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.createSharedFriendCard
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.makeSharedContactCode

/**
 * One contact's card, signed and handed over as a QR code
 * (specs/share-contact.md).
 *
 * A displayed code and nothing else: there is deliberately no **Copy link** and
 * no share intent (decision 2). Putting somebody else's name, keys, and their
 * family's mailbox deposit token into SMS or a group chat is a different act
 * from showing a code to the person standing in front of you, and only the
 * second one is what this screen is for.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ShareContactScreen(
    identity: Identity,
    contact: Contact,
    sharedPolicyRevision: ULong,
    onBack: () -> Unit,
) {
    val displayId = remember(contact.userId) { formatUserId(contact.userId) }
    // Decision 8: the card ships the contact's stored fields byte for byte --
    // never the sharer's own relay config, and never the nickname, which is
    // local presentation and was never part of any card.
    val shared = remember(contact, sharedPolicyRevision) {
        runCatching {
            createSharedFriendCard(
                identity,
                FriendCard(
                    name = contact.name,
                    signPk = contact.signPk,
                    agreePk = contact.agreePk,
                    relayUrl = contact.relayUrl,
                    relayToken = contact.relayToken,
                    // No primary self-signature when re-sharing: the sharer
                    // never holds the contact's signing key. Integrity of a
                    // shared card comes from the sharer's own SharedFriendCard
                    // signature instead.
                    signature = null,
                ),
                sharedPolicyRevision,
                System.currentTimeMillis(),
            )
        }.getOrNull()
    }
    val qrBitmap = remember(shared) {
        shared?.let { card -> runCatching { encodeQrBitmap(makeSharedContactCode(card)) }.getOrNull() }
    }
    val expiryDays = remember(shared) {
        shared?.let { ShareContactPolicy.daysUntil(it.expiresAtMs, System.currentTimeMillis()) } ?: 0
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_share_contact)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
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
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                contact.name,
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                displayId,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (qrBitmap != null) {
                Surface(
                    shape = RoundedCornerShape(20.dp),
                    color = Color.White,
                    tonalElevation = 2.dp,
                    shadowElevation = 2.dp,
                ) {
                    Image(
                        bitmap = qrBitmap,
                        contentDescription = stringResource(R.string.ui_shared_contact_qr_code),
                        modifier = Modifier
                            .padding(16.dp)
                            .size(240.dp),
                    )
                }
                Text(
                    pluralStringResource(
                        R.plurals.ui_share_code_explanation,
                        expiryDays,
                        contact.name,
                        expiryDays,
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            } else {
                Text(
                    stringResource(R.string.ui_share_code_too_large),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}
