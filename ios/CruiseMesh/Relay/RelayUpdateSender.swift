import Foundation
import os.log

/// T23 relay-change propagation, the exact twin of `ProfileSyncSender` and of
/// Android's `RelayUpdateSender.kt`.
///
/// A friend card is a *snapshot* of the sharer's relay config at the moment it
/// was shared. Buy a Cruise Pass, rotate a token, or migrate servers, and every
/// contact keeps posting to the endpoint they were handed — in the field that
/// looked like a phone posting to a long-retired host and collecting
/// `401 unknown family token` roughly ten times a minute, forever, while the
/// messages sat in the outbound queue showing a single tick. Nothing surfaced
/// it and the only repair was re-exchanging cards by hand.
///
/// The notice carries the **deposit** credential, never the member token: core's
/// `encodeRelayUpdateContent` attenuates whatever it is handed, so this file
/// passes the saved config through unmodified and cannot leak one (CP4).
enum RelayUpdateSender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "RelayUpdate")

    /// Fans the current endpoint out to every contact if it has changed since
    /// the last successful announcement.
    ///
    /// Driven from the relay sync pass, which every config-change path already
    /// ends in — so no save site has to remember to announce, and none can be
    /// missed. Idempotent, so the periodic poll re-entering here costs nothing.
    static func announceIfChanged(store: MessageStore, identity: Identity) {
        let epoch = RelayConfigStore.relayEpoch()
        guard epoch > RelayConfigStore.announcedRelayEpoch() else { return }
        // Our own mailbox moved (new pass, manual edit, restore): everything
        // "already uploaded" was confirmed against the OLD config, so
        // re-offer the whole carry queue once against the new one -- the
        // same wholesale clear core performs when a CONTACT's endpoint
        // moves (apply_contact_relay_update). Runs before this pass's
        // uploads, so the re-offer rides the very sync that detected the
        // change. Mirrors RelayUpdateSender.kt.
        _ = try? store.clearCarriedRelayUploadMarkers()
        queueToAllContacts(store: store, identity: identity, epoch: epoch)
        RelayConfigStore.markRelayEpochAnnounced(epoch)
    }

    static func queueToAllContacts(store: MessageStore, identity: Identity, epoch: Int64) {
        let relay = RelayConfigStore.load()
        // Blocked contacts get nothing from us — not even endpoint changes.
        let blocked = Set((try? store.listBlockedUsers()) ?? [])
        let contacts = ((try? store.listContacts()) ?? [])
            .filter { !blocked.contains($0.userId) }
        for contact in contacts {
            queueToContact(
                store: store,
                identity: identity,
                contact: contact,
                epoch: epoch,
                relay: relay
            )
        }
    }

    private static func queueToContact(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        epoch: Int64,
        relay: RelayConfig?
    ) {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        guard let payload = try? encodeRelayUpdateContent(content: RelayUpdateContent(
            // Only ever our own UserID: core rejects a notice whose subject is
            // not the sealing sender, so a third party's endpoint can never
            // ride along (endpoint privacy).
            subjectUserId: identity.userId,
            relayEpoch: epoch,
            // Empty when the pass lapsed or was removed — an honest "no
            // internet delivery any more", not a no-op.
            relayUrl: relay?.relayUrl ?? "",
            relayToken: relay?.relayToken ?? ""
        )) else { return }
        guard let authored = try? store.authorPairwiseMessage(
            identity: identity,
            contact: contact,
            kind: ProtocolKind.relayUpdate,
            payload: payload,
            replyToMsgId: nil,
            timestampMs: timestamp
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        RelaySyncEvents.requestSync()
        if !MeshRouter.sendToUserId(userId: contact.userId, frame: authored.frame) {
            log.info("Relay update queued for later delivery to \(contact.name, privacy: .public)")
        }
    }
}
