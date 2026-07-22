package com.cruisemesh.app.chat

import android.util.Log
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore

private const val TAG = "MeshSender"

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/** Cumulative-receipt type bytes (DESIGN.md §7.2), mirroring the private
 * copies already established in MainActivity.kt / ChatScreen.kt / MeshService.kt. */
/**
 * Sends an outgoing text or media message into a contact's 1:1 chat
 * (DESIGN.md §7.1 / §8). [ChatScreen] depends only on this interface -- never
 * on a concrete implementation -- so the transport can be swapped from a
 * local-only stub to real mesh delivery without any UI changes.
 */
interface MeshSender {
    /** Sends `text` as a `kind=1` message to `contact`'s chat. */
    fun sendText(contact: Contact, text: String, replyToMsgId: ByteArray? = null): SendResult

    /**
     * Sends an inline attachment as a `kind=16` chat-stream message
     * (attachment manifest with embedded blob; DESIGN.md §8).
     */
    fun sendAttachment(
        contact: Contact,
        attachment: AttachmentPayload,
        replyToMsgId: ByteArray? = null,
    ): SendResult

    /** Sends or clears this user's emoji reaction to [target]. */
    fun sendReaction(contact: Contact, target: MessageTarget, emoji: String): SendResult
}

/** Whether an authored message made it into the durable local message/outbound transaction. */
enum class SendResult {
    STORED,
    FAILED,
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
 * `identity.userId` is the sender. Rust atomically assigns Lamports and
 * ratchets past the peer's delivered/read watermarks.
 *
 * On the *wire*, `MessageBody.chatId` is set to `identity.userId` (our own),
 * not `contact.userId` -- see `MeshService`'s class KDoc for the full
 * explanation of why the wire and local conventions differ.
 */
class RealMeshSender(
    private val store: MessageStore,
    private val identity: Identity,
) : MeshSender {
    override fun sendText(contact: Contact, text: String, replyToMsgId: ByteArray?): SendResult =
        enqueueAuthored(
            contact = contact,
            kind = KIND_TEXT,
            payload = text.toByteArray(Charsets.UTF_8),
            logLabel = "sendText",
            replyToMsgId = replyToMsgId,
        )

    override fun sendAttachment(
        contact: Contact,
        attachment: AttachmentPayload,
        replyToMsgId: ByteArray?,
    ): SendResult {
        if (attachment.blob.size > AttachmentPayload.MAX_BLOB_BYTES) {
            Log.w(TAG, "Refusing attachment larger than ${AttachmentPayload.MAX_BLOB_BYTES} bytes")
            return SendResult.FAILED
        }
        val payload = try {
            attachment.encode()
        } catch (e: Exception) {
            Log.e(TAG, "sendAttachment: could not encode attachment", e)
            return SendResult.FAILED
        }
        return enqueueAuthored(
            contact = contact,
            kind = KIND_ATTACHMENT_MANIFEST,
            payload = payload,
            logLabel = "sendAttachment",
            replyToMsgId = replyToMsgId,
        )
    }

    override fun sendReaction(contact: Contact, target: MessageTarget, emoji: String): SendResult {
        val payload = try {
            ReactionPayload(target, emoji).encode()
        } catch (e: Exception) {
            Log.e(TAG, "sendReaction: could not encode reaction", e)
            return SendResult.FAILED
        }
        return enqueueAuthored(
            contact = contact,
            kind = KIND_REACTION,
            payload = payload,
            logLabel = "sendReaction",
        )
    }

    private fun enqueueAuthored(
        contact: Contact,
        kind: UByte,
        payload: ByteArray,
        logLabel: String,
        replyToMsgId: ByteArray? = null,
    ): SendResult {
        val queued = try {
            val timestamp = System.currentTimeMillis()
            val authored = store.authorPairwiseMessage(identity, contact, kind, payload, replyToMsgId, timestamp)
            // V2 field metric: note the outbound send so its delivery latency
            // and confirmation route can be measured on the cruise test.
            runCatching {
                store.recordSentMetric(authored.message.chatId, authored.message.lamport, timestamp)
            }
            authored.message.chatId to authored.acknowledgedDelivered
        } catch (e: Exception) {
            Log.e(TAG, "$logLabel: message was not stored for ${contact.name}", e)
            return SendResult.FAILED
        }

        val (chatId, ackedDelivered) = queued
        try {
            ChatEvents.notifyChatChanged(chatId)
            RelaySyncEvents.requestSync()

            // Preserve causal order during the one-sided friending window. The
            // friend-card request is an earlier row in this same authored stream.
            // If its first BLE attempt happened before the peer's link was ready,
            // sending only this new text lets the text arrive first as an unknown
            // sender; it then stays hidden until a reverse scan imports us. Replay
            // every still-unacknowledged envelope in lamport order so the card is
            // always queued on the link before the first visible message.
            val pending = store
                .outboundEnvelopesAfter(chatId, identity.userId, ackedDelivered)
                .sortedBy { it.lamport }
            for (pendingEnvelope in pending) {
                val frame = encodeOutboundEnvelopeFrame(pendingEnvelope)
                if (!MeshRouter.sendToUserId(contact.userId, frame)) {
                    // No direct link to the recipient right now -- give the sealed
                    // envelope to whoever IS connected so it can mule to the
                    // recipient later (muling hook A).
                    val muled = MeshRouter.relayToAll(frame)
                    Log.i(
                        TAG,
                        "$logLabel: ${contact.name} not currently connected; sprayed pending lamport=${pendingEnvelope.lamport} to $muled mule link(s)",
                    )
                }
            }
        } catch (e: Exception) {
            // The durable transaction already succeeded. A stale or failing
            // transport must leave the message visible and queued for retry.
            Log.w(TAG, "$logLabel: stored locally; immediate delivery will retry", e)
        }
        return SendResult.STORED
    }
}
