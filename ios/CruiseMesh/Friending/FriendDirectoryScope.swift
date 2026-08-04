import Foundation

/// Which contacts friends-of-friends introductions may involve: the ones on
/// our own Shore Pass, and nobody else. Mirrors Android's
/// `FriendDirectoryScope.kt`; the comparison itself lives in the core
/// (`relay_wire.rs::relay_contact_shares_own_family`).
///
/// Introductions spread along the contact graph, and the graph does not stop
/// at a household. One person who has scanned somebody outside the family is
/// enough for that outside circle to start appearing in family suggestion
/// lists — which is how a shared tester pass ends up offering strangers to a
/// child's phone. Nothing about it was a protocol failure; the pass simply
/// was never consulted.
///
/// A contact with no pass is not introducible either, and there is no
/// in-person fallback — the core function's doc explains why the signal was
/// too weak to keep. Without a pass, people add each other by scanning a code
/// or sharing their own friend link.
enum FriendDirectoryScope {

    /// Whether `contact` may be introduced with us at all.
    static func introducible(_ contact: Contact, ownRelay: RelayConfig?) -> Bool {
        relayContactSharesOwnFamily(
            contactRelayUrl: contact.relayUrl,
            contactRelayToken: contact.relayToken,
            ownRelayUrl: ownRelay?.relayUrl,
            ownRelayToken: ownRelay?.relayToken
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
        ownRelay: RelayConfig?
    ) -> [Contact] {
        guard introducible(recipient, ownRelay: ownRelay) else { return [] }
        return contacts.filter { candidate in
            candidate.userId != recipient.userId && introducible(candidate, ownRelay: ownRelay)
        }
    }
}
