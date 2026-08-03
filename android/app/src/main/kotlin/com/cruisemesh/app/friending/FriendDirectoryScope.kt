package com.cruisemesh.app.friending

import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.relayContactSharesOwnFamily

/**
 * Which contacts friends-of-friends introductions may involve: the ones on
 * our own Cruise Pass.
 *
 * Introductions spread along the contact graph, and the graph does not stop
 * at a household. One person who has scanned somebody outside the family is
 * enough for that outside circle to start appearing in family suggestion
 * lists — which is how a shared tester pass ends up offering strangers to a
 * child's phone. Nothing about it was a protocol failure; the pass simply was
 * never consulted.
 *
 * Scoping to the pass matches what a Cruise Pass already means everywhere
 * else in the app: the people you share a mailbox with are your family, and
 * everyone else is somebody you happen to know. Suggesting within that
 * boundary is the behavior the feature was pitched as.
 *
 * Kept free of Android imports so the policy is unit-testable on its own; the
 * pass comparison itself lives in the core
 * (`relay_wire.rs::relay_contact_shares_own_family`) and is shared with iOS.
 */
object FriendDirectoryScope {

    /**
     * Whether `contact` is on our pass. A contact with no pass of their own
     * counts as ours — see the core function's doc for why unknown is not
     * treated as foreign.
     */
    fun sharesOwnPass(contact: Contact, ownRelayUrl: String?, ownRelayToken: String?): Boolean =
        relayContactSharesOwnFamily(
            contact.relayUrl,
            contact.relayToken,
            ownRelayUrl,
            ownRelayToken,
        )

    /**
     * The candidates we may offer to `recipient`, given every contact we hold.
     *
     * Empty whenever the recipient is not on our pass: a snapshot is a list of
     * the people we know, so sending one off-pass would hand a family's names
     * to an outside circle — the same leak in the opposite direction.
     */
    fun candidatesFor(
        recipient: Contact,
        contacts: List<Contact>,
        ownRelayUrl: String?,
        ownRelayToken: String?,
    ): List<Contact> {
        if (!sharesOwnPass(recipient, ownRelayUrl, ownRelayToken)) return emptyList()
        return contacts.filter { candidate ->
            !candidate.userId.contentEquals(recipient.userId) &&
                sharesOwnPass(candidate, ownRelayUrl, ownRelayToken)
        }
    }
}
