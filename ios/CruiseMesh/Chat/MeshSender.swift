import Foundation
import os.log

protocol MeshSender {
    func sendText(contact: Contact, text: String)
    func sendAttachment(contact: Contact, attachment: AttachmentPayload)
    func sendReaction(contact: Contact, target: MessageTarget, emoji: String)
}

final class RealMeshSender: MeshSender {
    private let store: MessageStore
    private let identity: Identity
    private let log = Logger(subsystem: "com.cruisemesh", category: "MeshSender")

    init(store: MessageStore, identity: Identity) {
        self.store = store
        self.identity = identity
    }

    func sendText(contact: Contact, text: String) {
        sendText(contact: contact, text: text, replyToMsgId: nil)
    }

    func sendText(contact: Contact, text: String, replyToMsgId: Data?) {
        enqueue(
            contact: contact,
            kind: ProtocolKind.text,
            payload: Data(text.utf8),
            label: "sendText",
            replyToMsgId: replyToMsgId
        )
    }

    func sendAttachment(contact: Contact, attachment: AttachmentPayload) {
        sendAttachment(contact: contact, attachment: attachment, replyToMsgId: nil)
    }

    func sendAttachment(contact: Contact, attachment: AttachmentPayload, replyToMsgId: Data?) {
        guard attachment.blob.count <= AttachmentPayload.maxBlobBytes else {
            log.warning("Refusing oversized attachment")
            return
        }
        enqueue(
            contact: contact,
            kind: ProtocolKind.attachmentManifest,
            payload: attachment.encode(),
            label: "sendAttachment",
            replyToMsgId: replyToMsgId
        )
    }

    func sendReaction(contact: Contact, target: MessageTarget, emoji: String) {
        enqueue(
            contact: contact,
            kind: ProtocolKind.reaction,
            payload: ReactionPayload(target: target, emoji: emoji).encode(),
            label: "sendReaction"
        )
    }

    private func enqueue(
        contact: Contact,
        kind: UInt8,
        payload: Data,
        label: String,
        replyToMsgId: Data? = nil
    ) {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        guard let authored = try? store.authorPairwiseMessage(
            identity: identity, contact: contact, kind: kind, payload: payload,
            replyToMsgId: replyToMsgId, timestampMs: timestamp
        ) else {
            return
        }
        let chatId = authored.message.chatId
        let delivered = authored.acknowledgedDelivered
        ChatEvents.notifyChatChanged(chatId)
        RelaySyncEvents.requestSync()
        // A pending kind-3 friend card must reach the peer before the first
        // visible message, otherwise the message is stored as coming from an
        // unknown sender until a reverse scan. Replay the unacknowledged
        // authored stream in Lamport order on every new send.
        let pending = ((try? store.outboundEnvelopesAfter(
            chatId: chatId,
            senderUserId: identity.userId,
            afterLamport: delivered
        )) ?? []).sorted { $0.lamport < $1.lamport }
        for pendingEnvelope in pending {
            let frame = encodeOutboundEnvelopeFrame(pendingEnvelope)
            if !MeshRouter.sendToUserId(userId: contact.userId, frame: frame) {
                let muled = MeshRouter.relayToAll(frame: frame)
                log.info("\(label, privacy: .public): sprayed pending lamport \(pendingEnvelope.lamport) to \(muled) mule link(s)")
            }
        }
    }
}
