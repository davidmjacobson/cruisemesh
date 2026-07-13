import Foundation
import os.log

enum FriendRequestSender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "FriendRequest")

    /// After importing a scanned friend card, queue a signed kind=3 back.
    @discardableResult static func sendMutualFriendRequest(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        displayName: String
    ) -> FriendRequestDelivery {
        let cardJson = makeFriendCard(
            name: displayName.isEmpty ? "Friend" : displayName,
            identity: identity,
            relayUrl: RelayConfigStore.load()?.relayUrl,
            relayToken: RelayConfigStore.load()?.relayToken
        )
        let chatId = contact.userId
        let lamport = ((try? store.highestContiguousLamport(chatId: chatId, senderUserId: identity.userId)) ?? 0) + 1
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let message = StoredMessage(
            chatId: chatId,
            senderUserId: identity.userId,
            lamport: lamport,
            timestamp: timestamp,
            kind: ProtocolKind.friendRequest,
            payload: Data(cardJson.utf8)
        )
        guard let outbound = buildOutboundAuthoredEnvelope(identity: identity, contact: contact, message: message) else {
            return FriendRequestDelivery(reachedDirectly: false, lamport: lamport)
        }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        ChatEvents.notifyChatChanged(chatId)
        RelaySyncEvents.requestSync()
        let frame = encodeOutboundEnvelopeFrame(outbound)
        let reachedDirectly = MeshRouter.sendToUserId(userId: contact.userId, frame: frame)
        if !reachedDirectly {
            let muled = MeshRouter.relayToAll(frame: frame)
            log.info("Friend request queued for later delivery to \(contact.name, privacy: .public); sprayed to \(muled) mule link(s)")
        }
        return FriendRequestDelivery(reachedDirectly: reachedDirectly, lamport: lamport)
    }
}

struct FriendRequestDelivery: Hashable {
    let reachedDirectly: Bool
    let lamport: UInt64
}
