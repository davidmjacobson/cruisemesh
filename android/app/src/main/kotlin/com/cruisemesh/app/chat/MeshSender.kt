package com.cruisemesh.app.chat

import android.util.Log
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.buildOutboundAuthoredEnvelope
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage

private const val TAG = "MeshSender"

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/** Cumulative-receipt type bytes (DESIGN.md §7.2), mirroring the private
 * copies already established in MainActivity.kt / ChatScreen.kt / MeshService.kt. */
private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u

/**
 * Pure lamport-assignment logic for a locally authored message, split out so
 * the ratchet-against-receipts behavior is unit-testable without a native
 * [MessageStore] (mirrors [com.cruisemesh.app.relay.RelayImport.decide]).
 *
 * Picks one past the highest of: our own contiguous history, and whatever
 * cumulative "delivered through N" / "read through N" watermark the peer has
 * ever acked for our stream. The receipt watermarks matter because they are
 * replayed from the peer's persisted state on every reconnect and relay
 * sync (DESIGN.md §7.2/§7.3): if this chat was deleted and the contact
 * re-added, [ownContiguous] resets to 0 because our own history was wiped,
 * but the peer may still hold rows -- and a read watermark -- from the old
 * stream. Assigning a lamport at or below that watermark would land on a row
 * the peer already has: silently dropped by their `UNIQUE(chat_id,
 * sender_user_id, lamport)` dedup (core/src/store.rs), while their replayed
 * "read through N" receipt paints the new message ✓✓ (read) instantly, even
 * though it never arrived. Ratcheting past both watermarks keeps every
 * newly authored lamport strictly above anything the peer could already be
 * holding.
 */
fun nextAuthoredLamport(ownContiguous: ULong, ackedDelivered: ULong, ackedRead: ULong): ULong =
    maxOf(ownContiguous, ackedDelivered, ackedRead) + 1UL

/**
 * Sends an outgoing text or media message into a contact's 1:1 chat
 * (DESIGN.md §7.1 / §8). [ChatScreen] depends only on this interface -- never
 * on a concrete implementation -- so the transport can be swapped from a
 * local-only stub to real mesh delivery without any UI changes.
 */
interface MeshSender {
    /** Sends `text` as a `kind=1` message to `contact`'s chat. */
    fun sendText(contact: Contact, text: String)

    /**
     * Sends an inline attachment as a `kind=16` chat-stream message
     * (attachment manifest with embedded blob; DESIGN.md §8).
     */
    fun sendAttachment(contact: Contact, attachment: AttachmentPayload)
}

/**
 * Real [MeshSender]: seals the message once into a persistent outbound
 * envelope (DESIGN.md §4, §9), stores both the plaintext row and that exact
 * sealed retry copy atomically in [MessageStore], then transmits it
 * immediately if [MeshRouter] currently has a live link to the contact. If
 * the contact isn't connected right now, the same queued envelope stays local
 * for [com.cruisemesh.app.mesh.MeshService]'s digest sync (and later relay
 * upload) to retry. Because [ChatScreen] only ever depends on the
 * [MeshSender] interface, this swap-in required no UI changes.
 *
 * 1:1 chats use the peer's UserID as the *local* `chat_id` (DESIGN.md §7.1),
 * so the outgoing message is stored under `contact.userId` even though
 * `identity.userId` is the sender. Lamports come from [nextAuthoredLamport],
 * which is `highestContiguousLamport(...) + 1` in the common case -- safe
 * because our own outgoing stream is authored locally and therefore has no
 * gaps (DESIGN.md §7.1, §7.3), unlike a received stream, which needs the
 * real gap-aware sync logic instead (see `MeshService.handleIncomingText`)
 * -- but also ratchets past the peer's acked delivered/read watermarks, so a
 * chat that was deleted and its contact re-added (which wipes our own
 * history and resets our contiguous count) can't reissue a lamport the peer
 * still holds from before the wipe. See [nextAuthoredLamport]'s doc comment.
 *
 * On the *wire*, `MessageBody.chatId` is set to `identity.userId` (our own),
 * not `contact.userId` -- see `MeshService`'s class KDoc for the full
 * explanation of why the wire and local conventions differ.
 */
class RealMeshSender(
    private val store: MessageStore,
    private val identity: Identity,
) : MeshSender {
    override fun sendText(contact: Contact, text: String) {
        enqueueAuthored(
            contact = contact,
            kind = KIND_TEXT,
            payload = text.toByteArray(Charsets.UTF_8),
            logLabel = "sendText",
        )
    }

    override fun sendAttachment(contact: Contact, attachment: AttachmentPayload) {
        if (attachment.blob.size > AttachmentPayload.MAX_BLOB_BYTES) {
            Log.w(TAG, "Refusing attachment larger than ${AttachmentPayload.MAX_BLOB_BYTES} bytes")
            return
        }
        enqueueAuthored(
            contact = contact,
            kind = KIND_ATTACHMENT_MANIFEST,
            payload = attachment.encode(),
            logLabel = "sendAttachment",
        )
    }

    private fun enqueueAuthored(
        contact: Contact,
        kind: UByte,
        payload: ByteArray,
        logLabel: String,
    ) {
        val chatId = contact.userId
        val lamport = nextAuthoredLamport(
            ownContiguous = store.highestContiguousLamport(chatId, identity.userId),
            ackedDelivered = store.receiptThrough(chatId, identity.userId, RECEIPT_TYPE_DELIVERED),
            ackedRead = store.receiptThrough(chatId, identity.userId, RECEIPT_TYPE_READ),
        )
        val timestamp = System.currentTimeMillis()

        val message = StoredMessage(
            chatId = chatId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = kind,
            payload = payload,
        )
        val outbound = buildOutboundAuthoredEnvelope(identity, contact, message) ?: return
        store.insertOutgoingMessage(message, outbound, timestamp)
        ChatEvents.notifyChatChanged(chatId)
        RelaySyncEvents.requestSync()

        if (!MeshRouter.sendToUserId(contact.userId, encodeOutboundEnvelopeFrame(outbound))) {
            Log.i(TAG, "$logLabel: ${contact.name} not currently connected; message stays local until next digest sync")
        }
    }
}
