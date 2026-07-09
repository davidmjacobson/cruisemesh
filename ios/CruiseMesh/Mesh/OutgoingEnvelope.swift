import Foundation
import os.log

private let log = Logger(subsystem: "com.cruisemesh", category: "OutgoingEnvelope")

func buildOutboundAuthoredEnvelope(
    identity: Identity,
    contact: Contact,
    message: StoredMessage
) -> OutboundEnvelope? {
    guard isAuthoredChatKind(message.kind) else {
        log.warning("Refusing unsupported authored kind=\(message.kind)")
        return nil
    }
    let body = MessageBody(
        kind: message.kind,
        chatId: identity.userId, // wire convention: sender's own userId
        lamport: message.lamport,
        timestamp: message.timestamp,
        content: message.payload
    )
    do {
        let msgId = generateMsgId()
        GossipState.seenIds.record(msgId: msgId)
        let sealed = try sealMessage(
            sender: identity,
            recipientAgreePk: contact.agreePk,
            payload: encodeMessageBody(body: body)
        )
        return OutboundEnvelope(
            msgId: msgId,
            recipientUserId: contact.userId,
            chatId: message.chatId,
            senderUserId: message.senderUserId,
            kind: message.kind,
            lamport: message.lamport,
            timestamp: message.timestamp,
            hopTtl: MeshDefaults.hopTtl,
            expiry: defaultExpiry(timestampMs: message.timestamp),
            recipientHint: computeRecipientHint(recipientUserId: contact.userId, timestampMs: message.timestamp),
            sealed: sealed
        )
    } catch {
        log.warning("Failed to seal: \(error.localizedDescription, privacy: .public)")
        return nil
    }
}

func encodeOutboundEnvelopeFrame(_ envelope: OutboundEnvelope) -> Data {
    encodeEnvelopeFrame(
        msgId: envelope.msgId,
        hopTtl: envelope.hopTtl,
        expiry: envelope.expiry,
        recipientHint: envelope.recipientHint,
        sealed: envelope.sealed
    )
}
