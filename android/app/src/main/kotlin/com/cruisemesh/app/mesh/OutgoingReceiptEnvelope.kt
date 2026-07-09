package com.cruisemesh.app.mesh

import android.util.Log
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope
import uniffi.cruisemesh_core.ReceiptContent
import uniffi.cruisemesh_core.computeRecipientHint
import uniffi.cruisemesh_core.defaultExpiry
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.encodeReceiptContent
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.sealMessage

private const val TAG = "OutgoingReceiptEnv"

/** The `kind` byte for receipts (DESIGN.md §7.2). */
private const val KIND_RECEIPT: UByte = 2u

/** Mirrors core's `DEFAULT_HOP_TTL` for freshly authored outbound receipts. */
private const val DEFAULT_HOP_TTL: UByte = 7u

/**
 * Seals one cumulative receipt watermark into the persistent relay receipt
 * queue shape. Receipts are not part of a chat's lamport stream, so the
 * envelope body uses lamport=0 and carries the real cumulative watermark in
 * [ReceiptContent.lamport].
 */
fun buildOutgoingReceiptEnvelope(
    identity: Identity,
    contact: Contact,
    receiptType: UByte,
    ackedSenderUserId: ByteArray,
    throughLamport: ULong,
    timestamp: Long,
): OutgoingReceiptEnvelope? {
    val body = MessageBody(
        kind = KIND_RECEIPT,
        chatId = identity.userId,
        lamport = 0uL,
        timestamp = timestamp,
        content = encodeReceiptContent(
            ReceiptContent(
                chatId = identity.userId,
                senderUserId = ackedSenderUserId,
                lamport = throughLamport,
                receiptType = receiptType,
            ),
        ),
    )
    return try {
        val msgId = generateMsgId()
        GossipState.seenIds.record(msgId)
        OutgoingReceiptEnvelope(
            msgId = msgId,
            recipientUserId = contact.userId,
            chatId = contact.userId,
            senderUserId = ackedSenderUserId,
            receiptType = receiptType,
            throughLamport = throughLamport,
            timestamp = timestamp,
            hopTtl = DEFAULT_HOP_TTL,
            expiry = defaultExpiry(timestamp),
            recipientHint = computeRecipientHint(contact.userId, timestamp),
            sealed = sealMessage(identity, contact.agreePk, encodeMessageBody(body)),
        )
    } catch (e: CoreException) {
        Log.w(
            TAG,
            "Failed to seal outgoing receipt type=$receiptType through=$throughLamport for ${contact.name}: ${e.message}",
        )
        null
    }
}

/** Rebuilds the exact `0x02` frame bytes for one persisted outgoing receipt envelope. */
fun encodeOutgoingReceiptEnvelopeFrame(envelope: OutgoingReceiptEnvelope): ByteArray =
    encodeEnvelopeFrame(
        envelope.msgId,
        envelope.hopTtl,
        envelope.expiry,
        envelope.recipientHint,
        envelope.sealed,
    )
