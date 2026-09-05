import Foundation
import os.log

/// Whether an authored message made it into the durable local
/// message/outbound transaction. Mirrors Android's `SendResult`.
///
/// Without it every failure here was a `return` into `Void`, so the composer
/// could not tell "durably queued" from "gone" and cleared the user's typed
/// text either way — the silent send that lost the message.
enum SendResult: Equatable {
    case stored
    case failed
}

protocol MeshSender {
    @discardableResult
    func sendText(contact: Contact, text: String) -> SendResult
    @discardableResult
    func sendAttachment(contact: Contact, attachment: AttachmentPayload) -> SendResult
    @discardableResult
    func sendReaction(contact: Contact, target: MessageTarget, emoji: String) -> SendResult
}

final class RealMeshSender: MeshSender {
    private let store: MessageStore
    private let identity: Identity
    private let log = Logger(subsystem: "com.cruisemesh", category: "MeshSender")

    init(store: MessageStore, identity: Identity) {
        self.store = store
        self.identity = identity
    }

    @discardableResult
    func sendText(contact: Contact, text: String) -> SendResult {
        sendText(contact: contact, text: text, replyToMsgId: nil)
    }

    @discardableResult
    func sendText(contact: Contact, text: String, replyToMsgId: Data?) -> SendResult {
        enqueue(
            contact: contact,
            kind: ProtocolKind.text,
            payload: Data(text.utf8),
            label: "sendText",
            replyToMsgId: replyToMsgId
        )
    }

    @discardableResult
    func sendAttachment(contact: Contact, attachment: AttachmentPayload) -> SendResult {
        sendAttachment(contact: contact, attachment: attachment, replyToMsgId: nil)
    }

    @discardableResult
    func sendAttachment(contact: Contact, attachment: AttachmentPayload, replyToMsgId: Data?) -> SendResult {
        guard attachment.blob.count <= AttachmentPayload.maxBlobBytes else {
            log.warning("Refusing oversized attachment")
            return .failed
        }
        return enqueue(
            contact: contact,
            kind: ProtocolKind.attachmentManifest,
            payload: attachment.encode(),
            label: "sendAttachment",
            replyToMsgId: replyToMsgId
        )
    }

    @discardableResult
    func sendReaction(contact: Contact, target: MessageTarget, emoji: String) -> SendResult {
        guard let payload = ReactionPayload(target: target, emoji: emoji).encode() else { return .failed }
        return enqueue(
            contact: contact,
            kind: ProtocolKind.reaction,
            payload: payload,
            label: "sendReaction"
        )
    }

    @discardableResult
    private func enqueue(
        contact: Contact,
        kind: UInt8,
        payload: Data,
        label: String,
        replyToMsgId: Data? = nil
    ) -> SendResult {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let authored: AuthoredEnvelope
        do {
            authored = try store.authorPairwiseMessage(
                identity: identity, contact: contact, kind: kind, payload: payload,
                replyToMsgId: replyToMsgId, timestampMs: timestamp
            )
        } catch {
            // Nothing was stored and nothing was queued: say so, so the
            // composer keeps what the user typed. The core refuses to author
            // for real reasons — a contact whose keys will not seal, a device
            // still being adopted (§9.4) — and each of them used to end here
            // as a swallowed `try?`.
            log.error("\(label, privacy: .public): message was not stored: \(error.localizedDescription, privacy: .public)")
            return .failed
        }
        let chatId = authored.message.chatId
        let delivered = authored.acknowledgedDelivered
        // Everything below is best-effort delivery on top of a transaction that
        // already committed. It must never turn into `.failed`: the message is
        // stored and queued, and a retry would author it a second time.
        //
        // V2 field metric: note the outbound send so its delivery latency and
        // confirmation route can be measured on the cruise test.
        try? store.recordSentMetric(chatId: chatId, lamport: authored.message.lamport, sentAtMs: timestamp)
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
        return .stored
    }
}
