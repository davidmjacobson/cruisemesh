package com.cruisemesh.app.relay

import android.content.Context
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore

/** Where the persisted contact's relay fields should come from. */
enum class ContactRelaySource { CARD, EXISTING }

/** Pure outcome of reconciling an imported card's relay against local state. */
data class RelayImportDecision(
    val adoptAsFallback: Boolean,
    val contactSource: ContactRelaySource,
)

/**
 * Reconciles the relay fields of a contact imported from a friend card
 * (QR scan or an incoming friend request) against what this device already
 * knows, so relay config propagates the way people expect without a stray
 * empty card silently breaking things.
 *
 * Two behaviors, both keyed on whether the *card* actually carried a relay
 * (a non-blank URL and token):
 *
 * 1. **Adopt as our own fallback.** If the card carries a relay and this
 *    device has no [RelayConfigStore] fallback yet, save it. That is what lets
 *    a phone that was friended before relay setup start polling for *all* its
 *    own mail — and carry the relay onward in its own QR — from a single scan,
 *    instead of only being able to *reach* the scanned friend.
 * 2. **Never wipe an existing relay with a blank card.** [MessageStore.upsertContact]
 *    overwrites relay fields with whatever it is handed, so re-importing an
 *    older/blank card would otherwise clear a contact's working relay. When the
 *    card carries none, the persisted contact keeps the relay we already had.
 */
object RelayImport {
    /**
     * Pure decision, split out so the branching is unit-testable without an
     * Android [Context] or a native [MessageStore].
     */
    fun decide(
        cardHasRelay: Boolean,
        existingHasRelay: Boolean,
        hasOwnFallback: Boolean,
    ): RelayImportDecision =
        RelayImportDecision(
            adoptAsFallback = cardHasRelay && !hasOwnFallback,
            // Keep the existing relay only when the card brought none; otherwise
            // the card wins (fresh import, or an intentional relay change).
            contactSource = if (!cardHasRelay && existingHasRelay) {
                ContactRelaySource.EXISTING
            } else {
                ContactRelaySource.CARD
            },
        )

    fun reconcileOnImport(context: Context, store: MessageStore, incoming: Contact): Contact {
        val cardHasRelay = !incoming.relayUrl?.trim().isNullOrEmpty() &&
            !incoming.relayToken?.trim().isNullOrEmpty()
        val existing = store.getContact(incoming.userId)
        val existingHasRelay = !existing?.relayUrl?.trim().isNullOrEmpty() &&
            !existing?.relayToken?.trim().isNullOrEmpty()
        val hasOwnFallback = RelayConfigStore.load(context) != null

        val decision = decide(cardHasRelay, existingHasRelay, hasOwnFallback)

        if (decision.adoptAsFallback) {
            RelayConfigStore.save(
                context,
                relayUrl = incoming.relayUrl.orEmpty().trim(),
                relayToken = incoming.relayToken.orEmpty().trim(),
            )
        }
        return when (decision.contactSource) {
            ContactRelaySource.EXISTING ->
                incoming.copy(relayUrl = existing!!.relayUrl, relayToken = existing.relayToken)
            ContactRelaySource.CARD -> incoming
        }
    }
}
