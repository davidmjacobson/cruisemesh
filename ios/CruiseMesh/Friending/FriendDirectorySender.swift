import Foundation
import os.log

enum FriendDirectorySender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "FriendDirectory")
    private static let ticketLifetimeMs: Int64 = 30 * 24 * 60 * 60 * 1_000

    static func queueToAllContacts(store: MessageStore, identity: Identity) {
        // Blocked contacts are excluded both as directory recipients and as
        // candidates — we neither talk to them nor introduce them to friends.
        let blocked = Set((try? store.listBlockedUsers()) ?? [])
        let contacts = ((try? store.listContacts()) ?? [])
            .filter { !blocked.contains($0.userId) }
        let revision = FriendsOfFriendsStore.nextDirectoryRevision()
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let enabled = FriendsOfFriendsStore.isEnabled()
        let ownRelay = RelayConfigStore.load()

        // Off-pass recipients stay in this loop deliberately, receiving an
        // empty snapshot rather than being skipped. A newer empty revision is
        // the protocol's own retraction (specs/friends-of-friends.md), so a
        // phone already holding suggestions we should never have sent drops
        // them on the next pass instead of keeping them until the tickets
        // expire a month later. Mirrors FriendDirectorySender.kt.
        // Cached: with no pass on either side, eligibility turns on whether we
        // actually met, and the same contacts are re-checked once per
        // recipient across the whole fan-out.
        var nearbyCache: [Data: Bool] = [:]
        let addedNearby: (Data) -> Bool = { userId in
            if let known = nearbyCache[userId] { return known }
            let nearby = ((try? store.getContactProvenance(userId: userId)) ?? nil)?.addedNearby ?? false
            nearbyCache[userId] = nearby
            return nearby
        }
        for recipient in contacts {
            var entries: [FriendDirectoryEntry] = []
            if enabled {
                let eligible = FriendDirectoryScope.candidatesFor(
                    recipient: recipient,
                    contacts: contacts,
                    ownRelay: ownRelay,
                    addedNearby: addedNearby
                )
                for candidate in eligible {
                    guard entries.count < 64,
                          let policy = (try? store.getContactDiscoveryPolicy(userId: candidate.userId)) ?? nil,
                          policy.protocolVersion >= 1,
                          policy.enabled,
                          let ticket = try? createIntroductionTicket(
                            introducer: identity,
                            candidateUserId: candidate.userId,
                            inviteeUserId: recipient.userId,
                            candidatePolicyRevision: policy.revision,
                            issuedAtMs: now,
                            expiresAtMs: now + ticketLifetimeMs,
                            offerId: generateMsgId()
                          ) else { continue }
                    entries.append(FriendDirectoryEntry(
                        candidate: SuggestedFriendCard(
                            name: candidate.name,
                            userId: candidate.userId,
                            signPk: candidate.signPk,
                            agreePk: candidate.agreePk
                        ),
                        candidatePolicyRevision: policy.revision,
                        ticket: ticket
                    ))
                }
            }
            queue(
                store: store,
                identity: identity,
                recipient: recipient,
                kind: ProtocolKind.friendDirectory,
                payload: encodeFriendDirectoryContent(content: FriendDirectoryContent(
                    version: 1,
                    revision: revision,
                    entries: entries
                )),
                timestamp: now
            )
        }
    }

    @discardableResult static func requestSuggestedFriend(
        store: MessageStore,
        identity: Identity,
        displayName: String,
        suggestion: FriendSuggestion
    ) -> Bool {
        guard FriendsOfFriendsStore.isEnabled() else { return false }
        let candidate = Contact(
            userId: suggestion.candidate.userId,
            name: suggestion.candidate.name,
            signPk: suggestion.candidate.signPk,
            agreePk: suggestion.candidate.agreePk,
            relayUrl: nil,
            relayToken: nil
        )
        guard let card = try? makeFriendCard(
            name: displayName.isEmpty ? "Friend" : displayName,
            identity: identity,
            relayUrl: RelayConfigStore.load()?.relayUrl,
            relayToken: RelayConfigStore.load()?.relayToken
        ) else { return false }
        let queued = queue(
            store: store,
            identity: identity,
            recipient: candidate,
            kind: ProtocolKind.introducedFriendRequest,
            payload: encodeIntroducedFriendRequest(request: IntroducedFriendRequest(
                version: 1,
                friendCardJson: card,
                ticket: suggestion.ticket
            )),
            timestamp: Int64(Date().timeIntervalSince1970 * 1_000)
        )
        if queued { try? store.setFriendSuggestionState(candidateUserId: candidate.userId, state: 1) }
        return queued
    }

    @discardableResult private static func queue(
        store: MessageStore,
        identity: Identity,
        recipient: Contact,
        kind: UInt8,
        payload: Data,
        timestamp: Int64
    ) -> Bool {
        guard let authored = try? store.authorPairwiseMessage(
            identity: identity,
            contact: recipient,
            kind: kind,
            payload: payload,
            replyToMsgId: nil,
            timestampMs: timestamp
        ) else { return false }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        RelaySyncEvents.requestSync()
        let frame = authored.frame
        if !MeshRouter.sendToUserId(userId: recipient.userId, frame: frame) {
            let muled = MeshRouter.relayToAll(frame: frame)
            log.info("Queued hidden friend data for (recipient.name, privacy: .public); sprayed to (muled) mule(s)")
        }
        return true
    }
}
