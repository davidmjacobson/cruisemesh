package com.cruisemesh.app.friending

import android.content.Context
import com.cruisemesh.app.R
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayImport
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.ContactProvenance
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OutgoingSharedRequest
import uniffi.cruisemesh_core.SharedFriendCard
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.friendCardMatch
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.sharedCardExpired

/**
 * The one place a scanned or pasted card becomes a contact, so the scan route
 * and the paste route cannot drift on provenance, on the tombstone rules, or on
 * which mutual request gets sent back (specs/share-contact.md).
 */
object SharedCardImport {

    /**
     * Turns a scanned shared card into the confirmation the user answers.
     *
     * An expired card gets a literal message rather than a parse failure: an
     * expired share is the common case, not a malformed one, and "that doesn't
     * look like a friend code" would send somebody hunting for a typo.
     */
    fun previewShared(
        context: Context,
        store: MessageStore,
        ownUserId: ByteArray,
        shared: SharedFriendCard,
    ): ImportFriendResult {
        if (sharedCardExpired(shared, System.currentTimeMillis())) {
            return ImportFriendResult.Error(context.getString(R.string.ui_this_code_has_expired))
        }
        val userId = friendCardUserId(shared.card)
        if (userId.contentEquals(ownUserId)) {
            return ImportFriendResult.Error(context.getString(R.string.ui_thats_your_own_card))
        }
        val candidate = Contact(
            userId = userId,
            name = shared.card.name,
            signPk = shared.card.signPk,
            agreePk = shared.card.agreePk,
            relayUrl = shared.card.relayUrl,
            relayToken = shared.card.relayToken,
        )
        val sharerName = store.getContact(shared.sharerUserId)?.let(::coreContactDisplayName)
            ?: formatUserId(shared.sharerUserId)
        return ImportFriendResult.Preview(
            FriendPreview(
                contact = candidate,
                match = friendCardMatch(candidate, store.listContacts()),
                shared = shared,
                sharedByName = sharerName,
            ),
        )
    }

    /**
     * Imports a confirmed card and sends the mutual `kind=3` back.
     *
     * [addedNearby] is the caller's, because only the caller knows how the card
     * arrived: a camera scan is co-presence by construction, a pasted one says
     * nothing about where its owner is.
     */
    fun confirm(
        context: Context,
        store: MessageStore,
        identity: Identity,
        preview: FriendPreview,
        addedNearby: Boolean,
    ): FriendAddedOutcome {
        val contact = RelayImport.reconcileOnImport(context, store, preview.contact)
        val shared = preview.shared
        val now = System.currentTimeMillis()
        store.upsertContactProvenance(
            ContactProvenance(
                userId = contact.userId,
                source = if (shared == null) 0u else 2u,
                introducerUserId = shared?.sharerUserId,
                introducedAtMs = now,
                addedNearby = addedNearby,
            ),
        )
        store.removeFriendSuggestion(contact.userId)
        val delivery = if (shared == null) {
            // Scanning somebody's own code is the escape hatch that clears a
            // "don't ask again" tombstone, the same one friends-of-friends uses
            // for a deleted introduced contact.
            store.clearSharedRequestDismissal(contact.userId)
            FriendRequestSender.queueForScannedContact(context, store, identity, contact)
        } else {
            store.upsertOutgoingSharedRequest(
                OutgoingSharedRequest(
                    candidateUserId = contact.userId,
                    expiresAtMs = shared.expiresAtMs,
                    sentAtMs = now,
                ),
            )
            FriendRequestSender.queueForSharedCard(context, store, identity, contact, shared)
        }
        ProfileSyncSender.queueToContact(
            context,
            store,
            identity,
            contact,
            ProfileStore.loadOwnAvatarEpoch(context),
        )
        FriendDirectorySender.queueToAllContacts(context, store, identity)
        return FriendAddedOutcome(contact, delivery, RelayConfigStore.load(context) != null)
    }
}
