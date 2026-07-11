package com.cruisemesh.app.chat

import android.util.Log
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
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

    /** Sends or clears this user's emoji reaction to [target]. */
    fun sendReaction(contact: Contact, target: MessageTarget, emoji: String)
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
 * `identity.userId` is the sender. Computing `lamport` as
 * `highestContiguousLamport(...) + 1` is safe here specifically because our
 * own outgoing stream is authored locally and therefore has no gaps
 * (DESIGN.md §7.1, §7.3) -- a received stream needs the real gap-aware sync
 * logic instead (see `MeshService.handleIncomingText`).
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

    override fun sendReaction(contact: Contact, target: MessageTarget, emoji: String) {
        enqueueAuthored(
            contact = contact,
            kind = KIND_REACTION,
            payload = ReactionPayload(target, emoji).encode(),
            logLabel = "sendReaction",
        )
    }

    private fun enqueueAuthored(
        contact: Contact,
        kind: UByte,
        payload: ByteArray,
        logLabel: String,
    ) {
        val chatId = contact.userId
        val lamport = store.highestContiguousLamport(chatId, identity.userId) + 1UL
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
