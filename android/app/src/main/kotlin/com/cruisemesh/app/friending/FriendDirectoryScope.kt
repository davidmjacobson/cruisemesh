package com.cruisemesh.app.friending

import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.friendIntroductionEligible

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
 * everyone else is somebody you happen to know.
 *
 * Kept free of Android imports so the policy is unit-testable on its own; the
 * rule itself lives in the core
 * (`relay_wire.rs::friend_introduction_eligible`) and is shared with iOS,
 * including what an *absent* pass means on either side.
 */
object FriendDirectoryScope {

    /**
     * Whether `contact` may be introduced with us at all.
     *
     * [addedNearby] is `ContactProvenance.addedNearby` for this contact — it
     * only decides anything when neither side has a pass, where "did we
     * actually meet" is the only boundary left.
     */
    fun introducible(
        contact: Contact,
        ownRelayUrl: String?,
        ownRelayToken: String?,
        addedNearby: Boolean,
    ): Boolean =
        friendIntroductionEligible(
            contact.relayUrl,
            contact.relayToken,
            ownRelayUrl,
            ownRelayToken,
            addedNearby,
        )

    /**
     * The candidates we may offer to `recipient`, given every contact we hold.
     *
     * Empty whenever the recipient is not introducible: a snapshot is a list
     * of the people we know, so sending one to an outsider would hand a
     * family's names outward — the same leak in the opposite direction.
     *
     * [addedNearby] is looked up per contact rather than passed in bulk so
     * callers can back it with the provenance store directly.
     */
    fun candidatesFor(
        recipient: Contact,
        contacts: List<Contact>,
        ownRelayUrl: String?,
        ownRelayToken: String?,
        addedNearby: (ByteArray) -> Boolean,
    ): List<Contact> {
        if (!introducible(recipient, ownRelayUrl, ownRelayToken, addedNearby(recipient.userId))) {
            return emptyList()
        }
        return contacts.filter { candidate ->
            !candidate.userId.contentEquals(recipient.userId) &&
                introducible(candidate, ownRelayUrl, ownRelayToken, addedNearby(candidate.userId))
        }
    }
}
