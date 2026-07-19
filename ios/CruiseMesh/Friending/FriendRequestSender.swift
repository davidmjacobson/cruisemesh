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
        guard let cardJson = try? makeFriendCard(
            name: displayName.isEmpty ? "Friend" : displayName,
            identity: identity,
            relayUrl: RelayConfigStore.load()?.relayUrl,
            relayToken: RelayConfigStore.load()?.relayToken
        ) else {
            return FriendRequestDelivery(reachedDirectly: false, lamport: 0)
        }
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        guard let authored = try? store.authorFriendRequest(
            identity: identity, contact: contact, friendCardJson: cardJson, timestampMs: timestamp
        ) else {
            return FriendRequestDelivery(reachedDirectly: false, lamport: 0)
        }
        ChatEvents.notifyChatChanged(authored.message.chatId)
        RelaySyncEvents.requestSync()
        let reachedDirectly = MeshRouter.sendToUserId(userId: contact.userId, frame: authored.frame)
        if !reachedDirectly {
            let muled = MeshRouter.relayToAll(frame: authored.frame)
            log.info("Friend request queued for later delivery to \(contact.name, privacy: .public); sprayed to \(muled) mule link(s)")
        }
        return FriendRequestDelivery(reachedDirectly: reachedDirectly, lamport: authored.message.lamport)
    }
}

struct FriendRequestDelivery: Hashable {
    let reachedDirectly: Bool
    let lamport: UInt64
}
