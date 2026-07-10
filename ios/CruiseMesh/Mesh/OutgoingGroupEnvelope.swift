import Foundation
import os.log

private let groupLog = Logger(subsystem: "com.cruisemesh", category: "OutgoingGroupEnvelope")

/// Seals one locally authored group text message with the shared group key
/// (DESIGN.md §6.5). The public header's `recipient_hint` hashes the group id
/// (same `computeRecipientHint` as 1:1, keyed on the group id rather than a
/// user id) so every member can recognize and open the envelope.
///
/// `OutboundEnvelope.recipientUserId` is set to the group id: the queue's
/// dedupe key includes that field, and for group-authored traffic there is
/// exactly one sealed copy for all members (unlike kind=4 invites).
func buildOutboundGroupEnvelope(
    identity: Identity,
    group: Group,
    message: StoredMessage
) -> OutboundEnvelope? {
    guard message.kind == ProtocolKind.text else {
        groupLog.warning("Refusing to queue unsupported group message kind=\(message.kind)")
        return nil
    }
    guard message.chatId == group.id else {
        groupLog.warning("Refusing group envelope whose chatId does not match group.id")
        return nil
    }
    guard group.memberUserIds.contains(identity.userId) else {
        groupLog.warning("Refusing group envelope from a non-member identity")
        return nil
    }

    let body = MessageBody(
        kind: message.kind,
        chatId: group.id, // group traffic uses the group id on the wire (DESIGN.md §7.1)
        lamport: message.lamport,
        timestamp: message.timestamp,
        content: message.payload
    )
    do {
        let msgId = generateMsgId()
        GossipState.seenIds.record(msgId: msgId)
        let sealed = try sealGroupMessage(
            sender: identity,
            group: group,
            payload: encodeMessageBody(body: body)
        )
        return OutboundEnvelope(
            msgId: msgId,
            recipientUserId: group.id,
            chatId: message.chatId,
            senderUserId: message.senderUserId,
            kind: message.kind,
            lamport: message.lamport,
            timestamp: message.timestamp,
            hopTtl: MeshDefaults.hopTtl,
            expiry: defaultExpiry(timestampMs: message.timestamp),
            recipientHint: computeRecipientHint(recipientUserId: group.id, timestampMs: message.timestamp),
            sealed: sealed
        )
    } catch {
        groupLog.warning("Failed to seal group message for \(group.name, privacy: .public): \(error.localizedDescription, privacy: .public)")
        return nil
    }
}
