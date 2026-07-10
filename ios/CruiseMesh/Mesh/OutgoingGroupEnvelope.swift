import Foundation
import os.log

private let groupEnvelopeLog = Logger(subsystem: "com.cruisemesh", category: "OutgoingGroupEnvelope")

func buildOutboundGroupEnvelope(
    identity: Identity,
    group: Group,
    message: StoredMessage
) -> OutboundEnvelope? {
    guard message.kind == ProtocolKind.text else {
        groupEnvelopeLog.warning("Refusing unsupported group kind=\(message.kind)")
        return nil
    }
    guard message.chatId == group.id else {
        groupEnvelopeLog.warning("Refusing group envelope with mismatched chat id")
        return nil
    }
    guard group.memberUserIds.contains(identity.userId) else {
        groupEnvelopeLog.warning("Refusing group envelope from a non-member identity")
        return nil
    }

    let body = MessageBody(
        kind: message.kind,
        chatId: group.id,
        lamport: message.lamport,
        timestamp: message.timestamp,
        content: message.payload
    )
    do {
        let msgId = generateMsgId()
        GossipState.seenIds.record(msgId: msgId)
        return OutboundEnvelope(
            msgId: msgId,
            recipientUserId: group.id,
            chatId: group.id,
            senderUserId: identity.userId,
            kind: message.kind,
            lamport: message.lamport,
            timestamp: message.timestamp,
            hopTtl: MeshDefaults.hopTtl,
            expiry: defaultExpiry(timestampMs: message.timestamp),
            recipientHint: computeRecipientHint(recipientUserId: group.id, timestampMs: message.timestamp),
            sealed: try sealGroupMessage(
                sender: identity,
                group: group,
                payload: encodeMessageBody(body: body)
            )
        )
    } catch {
        groupEnvelopeLog.warning("Failed to seal group message: \(error.localizedDescription, privacy: .public)")
        return nil
    }
}
