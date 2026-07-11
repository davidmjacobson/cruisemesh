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
        enqueue(contact: contact, kind: ProtocolKind.text, payload: Data(text.utf8), label: "sendText")
    }

    func sendAttachment(contact: Contact, attachment: AttachmentPayload) {
        guard attachment.blob.count <= AttachmentPayload.maxBlobBytes else {
            log.warning("Refusing oversized attachment")
            return
        }
        enqueue(
            contact: contact,
            kind: ProtocolKind.attachmentManifest,
            payload: attachment.encode(),
            label: "sendAttachment"
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

    private func enqueue(contact: Contact, kind: UInt8, payload: Data, label: String) {
        let chatId = contact.userId
        let lamport = (try? store.highestContiguousLamport(chatId: chatId, senderUserId: identity.userId)) ?? 0
        let next = lamport + 1
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let message = StoredMessage(
            chatId: chatId,
            senderUserId: identity.userId,
            lamport: next,
            timestamp: timestamp,
            kind: kind,
            payload: payload
        )
        guard let outbound = buildOutboundAuthoredEnvelope(identity: identity, contact: contact, message: message) else {
            return
        }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        ChatEvents.notifyChatChanged(chatId)
        RelaySyncEvents.requestSync()
        if !MeshRouter.sendToUserId(userId: contact.userId, frame: encodeOutboundEnvelopeFrame(outbound)) {
            log.info("\(label, privacy: .public): \(contact.name, privacy: .public) not connected; queued")
        }
    }
}
