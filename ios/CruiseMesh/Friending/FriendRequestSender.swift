import Foundation
import os.log

enum FriendRequestSender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "FriendRequest")

    /// After importing a scanned friend card, queue a signed kind=3 back.
    ///
    /// `shared` is set only when the card we just imported came out of somebody
    /// else's **Share contact** code (specs/share-contact.md). It rides along as
    /// a backwards-decodable tail so the other phone can tell an introduction
    /// from a scan and ask before importing; everything else about the send is
    /// identical, which is why this is one parameter rather than a second path.
    @discardableResult static func sendMutualFriendRequest(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        displayName: String,
        shared: SharedFriendCard? = nil
    ) -> FriendRequestDelivery {
        guard let ownCardJson = try? makeFriendCard(
            name: displayName.isEmpty ? "Friend" : displayName,
            identity: identity,
            relayUrl: RelayConfigStore.load()?.relayUrl,
            relayToken: RelayConfigStore.load()?.relayToken
        ) else {
            return FriendRequestDelivery(reachedDirectly: false, lamport: 0)
        }
        let cardJson: String
        if let shared {
            // Fail rather than silently sending a tailless request: that would
            // land as an auto-import on their phone, which is exactly the
            // confirmation this flow exists to ask for.
            guard let tailed = try? makeSharedFriendRequestPayload(
                cardJson: ownCardJson,
                shared: shared
            ) else {
                return FriendRequestDelivery(reachedDirectly: false, lamport: 0)
            }
            cardJson = tailed
        } else {
            cardJson = ownCardJson
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
            log.info("Friend request queued for later delivery to \(UserIdHex.encode(contact.userId), privacy: .public); sprayed to \(muled) mule link(s)")
        }
        return FriendRequestDelivery(reachedDirectly: reachedDirectly, lamport: authored.message.lamport)
    }
}

struct FriendRequestDelivery: Hashable {
    let reachedDirectly: Bool
    let lamport: UInt64
}
