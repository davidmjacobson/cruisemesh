import Foundation
import os.log

enum FriendDirectorySender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "FriendDirectory")
    private static let ticketLifetimeMs: Int64 = 30 * 24 * 60 * 60 * 1_000

    static func queueToAllContacts(store: MessageStore, identity: Identity) {
        let contacts = (try? store.listContacts()) ?? []
        let revision = FriendsOfFriendsStore.nextDirectoryRevision()
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let enabled = FriendsOfFriendsStore.isEnabled()

        for recipient in contacts {
            var entries: [FriendDirectoryEntry] = []
            if enabled {
                for candidate in contacts where candidate.userId != recipient.userId {
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
        let card = makeFriendCard(
            name: displayName.isEmpty ? "Friend" : displayName,
            identity: identity,
            relayUrl: RelayConfigStore.load()?.relayUrl,
            relayToken: RelayConfigStore.load()?.relayToken
        )
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
        let chatId = recipient.userId
        let own = (try? store.highestContiguousLamport(chatId: chatId, senderUserId: identity.userId)) ?? 0
        let delivered = (try? store.receiptThrough(
            chatId: chatId,
            senderUserId: identity.userId,
            receiptType: ReceiptType.delivered
        )) ?? 0
        let read = (try? store.receiptThrough(
            chatId: chatId,
            senderUserId: identity.userId,
            receiptType: ReceiptType.read
        )) ?? 0
        let message = StoredMessage(
            chatId: chatId,
            senderUserId: identity.userId,
            lamport: max(own, delivered, read) + 1,
            timestamp: timestamp,
            kind: kind,
            payload: payload
        )
        guard let outbound = buildOutboundAuthoredEnvelope(
            identity: identity,
            contact: recipient,
            message: message
        ) else { return false }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        RelaySyncEvents.requestSync()
        let frame = encodeOutboundEnvelopeFrame(outbound)
        if !MeshRouter.sendToUserId(userId: recipient.userId, frame: frame) {
            let muled = MeshRouter.relayToAll(frame: frame)
            log.info("Queued hidden friend data for (recipient.name, privacy: .public); sprayed to (muled) mule(s)")
        }
        return true
    }
}
