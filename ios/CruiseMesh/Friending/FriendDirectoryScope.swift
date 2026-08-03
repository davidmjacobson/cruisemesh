import Foundation

/// Which contacts friends-of-friends introductions may involve: the ones on
/// our own Cruise Pass. Mirrors Android's `FriendDirectoryScope.kt`; the pass
/// comparison itself lives in the core
/// (`relay_wire.rs::relay_contact_shares_own_family`).
///
/// Introductions spread along the contact graph, and the graph does not stop
/// at a household. One person who has scanned somebody outside the family is
/// enough for that outside circle to start appearing in family suggestion
/// lists — which is how a shared tester pass ends up offering strangers to a
/// child's phone. Nothing about it was a protocol failure; the pass simply
/// was never consulted.
enum FriendDirectoryScope {

    /// Whether `contact` is on our pass. A contact with no pass of their own
    /// counts as ours — see the core function's doc for why unknown is not
    /// treated as foreign.
    static func sharesOwnPass(_ contact: Contact, ownRelay: RelayConfig?) -> Bool {
        relayContactSharesOwnFamily(
            contactRelayUrl: contact.relayUrl,
            contactRelayToken: contact.relayToken,
            ownRelayUrl: ownRelay?.relayUrl,
            ownRelayToken: ownRelay?.relayToken
        )
    }

    /// The candidates we may offer to `recipient`, given every contact we hold.
    ///
    /// Empty whenever the recipient is not on our pass: a snapshot is a list
    /// of the people we know, so sending one off-pass would hand a family's
    /// names to an outside circle — the same leak in the opposite direction.
    static func candidatesFor(
        recipient: Contact,
        contacts: [Contact],
        ownRelay: RelayConfig?
    ) -> [Contact] {
        guard sharesOwnPass(recipient, ownRelay: ownRelay) else { return [] }
        return contacts.filter { candidate in
            candidate.userId != recipient.userId && sharesOwnPass(candidate, ownRelay: ownRelay)
        }
    }
}
