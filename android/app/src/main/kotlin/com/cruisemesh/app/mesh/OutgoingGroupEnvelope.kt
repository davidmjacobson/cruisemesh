package com.cruisemesh.app.mesh

import android.util.Log
import com.cruisemesh.app.media.KIND_REACTION
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.defaultExpiry
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.sealGroupMessage

private const val TAG = "OutgoingGroupEnvelope"

/** Mirrors core's `DEFAULT_HOP_TTL` for freshly authored outbound messages. */
private const val DEFAULT_HOP_TTL: UByte = 7u

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: UByte = 1u

private fun isAuthoredGroupKind(kind: UByte): Boolean =
    kind == KIND_TEXT || kind == KIND_REACTION

/**
 * Seals one locally authored group text message with the shared group key
 * (DESIGN.md §6.5). The public header's `recipient_hint` hashes the group id
 * (same [computeRecipientHint] as 1:1, keyed on group id rather than a user
 * id) so every member can recognize and open the envelope.
 *
 * [OutboundEnvelope.recipientUserId] is set to the group id: the queue's
 * dedupe key includes that field, and for group-authored traffic there is
 * exactly one sealed copy for all members (unlike kind=4 invites).
 */
fun buildOutboundGroupEnvelope(
    identity: Identity,
    group: Group,
    message: StoredMessage,
): OutboundEnvelope? {
    if (!isAuthoredGroupKind(message.kind)) {
        Log.w(TAG, "Refusing to queue unsupported group message kind=${message.kind}")
        return null
    }
    if (!message.chatId.contentEquals(group.id)) {
        Log.w(TAG, "Refusing group envelope whose chatId does not match group.id")
        return null
    }

    val body = MessageBody(
        kind = message.kind,
        // Group traffic uses the group id on the wire (DESIGN.md §7.1).
        chatId = group.id,
        lamport = message.lamport,
        timestamp = message.timestamp,
        content = message.payload,
    )
    return try {
        val msgId = generateMsgId()
        GossipState.seenIds.record(msgId)
        OutboundEnvelope(
            msgId = msgId,
            recipientUserId = group.id,
            chatId = message.chatId,
            senderUserId = message.senderUserId,
            kind = message.kind,
            lamport = message.lamport,
            timestamp = message.timestamp,
            hopTtl = DEFAULT_HOP_TTL,
            expiry = defaultExpiry(message.timestamp),
            recipientHint = computeRecipientHint(group.id, message.timestamp),
            sealed = sealGroupMessage(identity, group, encodeMessageBody(body)),
        )
    } catch (e: CoreException) {
        Log.w(TAG, "Failed to seal group message for ${group.name}: ${e.message}")
        null
    }
}
