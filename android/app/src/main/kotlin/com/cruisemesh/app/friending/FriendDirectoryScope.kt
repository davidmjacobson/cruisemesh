package com.cruisemesh.app.friending

import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.relayContactSharesOwnFamily

/**
 * Which contacts friends-of-friends introductions may involve: the ones on
 * our own Shore Pass, and nobody else.
 *
 * Introductions spread along the contact graph, and the graph does not stop
 * at a household. One person who has scanned somebody outside the family is
 * enough for that outside circle to start appearing in family suggestion
 * lists — which is how a shared tester pass ends up offering strangers to a
 * child's phone. Nothing about it was a protocol failure; the pass simply was
 * never consulted.
 *
 * A contact with no pass is not introducible either, and there is no
 * in-person fallback — the core function's doc explains why the signal was
 * too weak to keep. Without a pass, people add each other by scanning a code
 * or sharing their own friend link.
 *
 * Kept free of Android imports so the policy is unit-testable on its own; the
 * comparison itself lives in the core
 * (`relay_wire.rs::relay_contact_shares_own_family`) and is shared with iOS.
 */
object FriendDirectoryScope {

    /** Whether `contact` may be introduced with us at all. */
    fun introducible(contact: Contact, ownRelayUrl: String?, ownRelayToken: String?): Boolean =
        relayContactSharesOwnFamily(
            contact.relayUrl,
            contact.relayToken,
            ownRelayUrl,
            ownRelayToken,
        )

    /**
     * The candidates we may offer to `recipient`, given every contact we hold.
     *
     * Empty whenever the recipient is not introducible: a snapshot is a list
     * of the people we know, so sending one to an outsider would hand a
     * family's names outward — the same leak in the opposite direction.
     */
    fun candidatesFor(
        recipient: Contact,
        contacts: List<Contact>,
        ownRelayUrl: String?,
        ownRelayToken: String?,
    ): List<Contact> {
        if (!introducible(recipient, ownRelayUrl, ownRelayToken)) return emptyList()
        return contacts.filter { candidate ->
            !candidate.userId.contentEquals(recipient.userId) &&
                introducible(candidate, ownRelayUrl, ownRelayToken)
        }
    }
}
