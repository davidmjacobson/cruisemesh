import Foundation

/// Which contacts friends-of-friends introductions may involve: the ones on
/// our own Cruise Pass. Mirrors Android's `FriendDirectoryScope.kt`; the rule
/// itself lives in the core (`relay_wire.rs::friend_introduction_eligible`),
/// including what an *absent* pass means on either side.
///
/// Introductions spread along the contact graph, and the graph does not stop
/// at a household. One person who has scanned somebody outside the family is
/// enough for that outside circle to start appearing in family suggestion
/// lists — which is how a shared tester pass ends up offering strangers to a
/// child's phone. Nothing about it was a protocol failure; the pass simply
/// was never consulted.
enum FriendDirectoryScope {

    /// Whether `contact` may be introduced with us at all.
    ///
    /// `addedNearby` is `ContactProvenance.addedNearby` for this contact — it
    /// only decides anything when neither side has a pass, where "did we
    /// actually meet" is the only boundary left.
    static func introducible(
        _ contact: Contact,
        ownRelay: RelayConfig?,
        addedNearby: Bool
    ) -> Bool {
        friendIntroductionEligible(
            contactRelayUrl: contact.relayUrl,
            contactRelayToken: contact.relayToken,
            ownRelayUrl: ownRelay?.relayUrl,
            ownRelayToken: ownRelay?.relayToken,
            contactAddedNearby: addedNearby
        )
    }

    /// The candidates we may offer to `recipient`, given every contact we hold.
    ///
    /// Empty whenever the recipient is not introducible: a snapshot is a list
    /// of the people we know, so sending one to an outsider would hand a
    /// family's names outward — the same leak in the opposite direction.
    static func candidatesFor(
        recipient: Contact,
        contacts: [Contact],
        ownRelay: RelayConfig?,
        addedNearby: (Data) -> Bool
    ) -> [Contact] {
        guard introducible(recipient, ownRelay: ownRelay, addedNearby: addedNearby(recipient.userId))
        else { return [] }
        return contacts.filter { candidate in
            candidate.userId != recipient.userId
                && introducible(
                    candidate,
                    ownRelay: ownRelay,
                    addedNearby: addedNearby(candidate.userId)
                )
        }
    }
}
