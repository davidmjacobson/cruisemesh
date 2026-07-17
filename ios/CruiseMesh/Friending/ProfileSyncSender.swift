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
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let payload = encodeProfileSyncContent(content: ProfileSyncContent(
            avatarEpoch: epoch,
            name: displayName.isEmpty ? "Friend" : displayName,
            avatar: avatar,
            friendsOfFriendsVersion: 1,
            friendsOfFriendsEnabled: FriendsOfFriendsStore.isEnabled(),
            friendsOfFriendsRevision: FriendsOfFriendsStore.revision()
        ))
        guard let authored = try? store.authorPairwiseMessage(
            identity: identity,
            contact: contact,
            kind: ProtocolKind.profileSync,
            payload: payload,
            replyToMsgId: nil,
            timestampMs: timestamp
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        RelaySyncEvents.requestSync()
        if !MeshRouter.sendToUserId(userId: contact.userId, frame: authored.frame) {
            log.info("Profile sync queued for later delivery to \(contact.name, privacy: .public)")
        }
    }
}
