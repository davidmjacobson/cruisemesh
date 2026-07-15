import Foundation
import os.log

enum ProfileSyncSender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "ProfileSync")

    static func queueToAllContacts(
        store: MessageStore,
        identity: Identity,
        displayName: String,
        epoch: Int64
    ) {
        let avatar = ProfilePhotoStore.loadWireAvatarBytes()
        let contacts = (try? store.listContacts()) ?? []
        for contact in contacts {
            queueToContact(
                store: store,
                identity: identity,
                contact: contact,
                displayName: displayName,
                epoch: epoch,
                avatar: avatar
            )
        }
    }

    static func queueToContact(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        displayName: String,
        epoch: Int64
    ) {
        queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            displayName: displayName,
            epoch: epoch,
            avatar: ProfilePhotoStore.loadWireAvatarBytes()
        )
    }

    private static func queueToContact(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        displayName: String,
        epoch: Int64,
        avatar: Data
    ) {
        let chatId = contact.userId
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
        let lamport = max(own, delivered, read) + 1
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let payload = encodeProfileSyncContent(content: ProfileSyncContent(
            avatarEpoch: epoch,
            name: displayName.isEmpty ? "Friend" : displayName,
            avatar: avatar,
            friendsOfFriendsVersion: 1,
            friendsOfFriendsEnabled: FriendsOfFriendsStore.isEnabled(),
            friendsOfFriendsRevision: FriendsOfFriendsStore.revision()
        ))
        let message = StoredMessage(
            chatId: chatId,
            senderUserId: identity.userId,
            lamport: lamport,
            timestamp: timestamp,
            kind: ProtocolKind.profileSync,
            payload: payload
        )
        guard let outbound = buildOutboundAuthoredEnvelope(identity: identity, contact: contact, message: message) else {
            return
        }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        RelaySyncEvents.requestSync()
        if !MeshRouter.sendToUserId(userId: contact.userId, frame: encodeOutboundEnvelopeFrame(outbound)) {
            log.info("Profile sync queued for later delivery to \(contact.name, privacy: .public)")
        }
    }
}
