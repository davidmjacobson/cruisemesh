package com.cruisemesh.app.mesh

import android.util.Log
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.defaultExpiry
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.sealMessage

private const val TAG = "OutgoingEnvelope"

/** The `kind` bytes for authored chat-stream messages (DESIGN.md §7.1). */
private const val KIND_TEXT: UByte = 1u
private const val KIND_FRIEND_REQUEST: UByte = 3u
private const val KIND_GROUP_INVITE: UByte = 4u

/** Mirrors core's `DEFAULT_HOP_TTL` for freshly authored outbound messages. */
private const val DEFAULT_HOP_TTL: UByte = 7u

private fun isAuthoredChatKind(kind: UByte): Boolean =
    kind == KIND_TEXT ||
        kind == KIND_FRIEND_REQUEST ||
        kind == KIND_GROUP_INVITE ||
        kind == KIND_ATTACHMENT_MANIFEST ||
        kind == KIND_REACTION

/**
 * Seals one locally authored chat-stream message into the persistent outbound
 * queue shape used by BLE retry and relay upload. The message's own authoring
 * timestamp drives both `recipient_hint` and `expiry`, so every resend
 * preserves the exact same §6.4 public header instead of drifting by
 * reconnect time.
 */
fun buildOutboundAuthoredEnvelope(
    identity: Identity,
    contact: Contact,
    message: StoredMessage,
): OutboundEnvelope? {
    if (!isAuthoredChatKind(message.kind)) {
        Log.w(TAG, "Refusing to queue unsupported authored message kind=${message.kind}")
        return null
    }

    val body = MessageBody(
        kind = message.kind,
        chatId = identity.userId, // wire convention: sender's own userId -- see MeshService KDoc
        lamport = message.lamport,
        timestamp = message.timestamp,
        content = message.payload,
    )
    return try {
        val msgId = generateMsgId()
        // Remember our own msg_id so if the mesh later floods this envelope
        // back to us we drop it as a duplicate instead of treating it as
        // foreign traffic.
        GossipState.seenIds.record(msgId)
        OutboundEnvelope(
            msgId = msgId,
            recipientUserId = contact.userId,
            chatId = message.chatId,
            senderUserId = message.senderUserId,
            kind = message.kind,
            lamport = message.lamport,
            timestamp = message.timestamp,
            hopTtl = DEFAULT_HOP_TTL,
            expiry = defaultExpiry(message.timestamp),
            recipientHint = computeRecipientHint(contact.userId, message.timestamp),
            sealed = sealMessage(identity, contact.agreePk, encodeMessageBody(body)),
        )
    } catch (e: CoreException) {
        Log.w(TAG, "Failed to seal outgoing kind=${message.kind} for ${contact.name}: ${e.message}")
        null
    }
}

/** Rebuilds the exact `0x02` frame bytes for one persisted outbound envelope. */
fun encodeOutboundEnvelopeFrame(envelope: OutboundEnvelope): ByteArray =
    encodeEnvelopeFrame(
        envelope.msgId,
        envelope.hopTtl,
        envelope.expiry,
        envelope.recipientHint,
        envelope.sealed,
    )
