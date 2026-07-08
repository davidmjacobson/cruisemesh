package com.cruisemesh.app.chat

import android.util.Log
import com.cruisemesh.app.mesh.MeshRouter
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.encodeMessageBody
import uniffi.cruisemesh_core.sealMessage

private const val TAG = "MeshSender"

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/**
 * Sends an outgoing text message into a contact's 1:1 chat (DESIGN.md §7.1).
 * [ChatScreen] depends only on this interface -- never on a concrete
 * implementation -- so the transport can be swapped from a local-only stub
 * to real mesh delivery without any UI changes.
 */
interface MeshSender {
    /** Sends `text` as a `kind=1` message to `contact`'s chat. */
    fun sendText(contact: Contact, text: String)
}

/**
 * Real [MeshSender]: persists the outgoing message into the local
 * [MessageStore] exactly as the Milestone-1 placeholder (`LocalEchoSender`)
 * used to, then -- if [MeshRouter] currently has a live link to that contact
 * -- seals it (DESIGN.md §6.3) and transmits it immediately (DESIGN.md
 * §5.2). If the contact isn't connected right now, the message simply stays
 * local; [com.cruisemesh.app.mesh.MeshService]'s digest sync (DESIGN.md
 * §7.3) delivers it whenever that contact is next seen and HELLOs in.
 * Because [ChatScreen] only ever depends on the [MeshSender] interface, this
 * swap-in required no UI changes.
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
        val chatId = contact.userId
        val lamport = store.highestContiguousLamport(chatId, identity.userId) + 1UL
        val timestamp = System.currentTimeMillis()
        val payload = text.toByteArray(Charsets.UTF_8)

        store.insertMessage(
            StoredMessage(
                chatId = chatId,
                senderUserId = identity.userId,
                lamport = lamport,
                timestamp = timestamp,
                kind = KIND_TEXT,
                payload = payload,
            ),
        )
        ChatEvents.notifyChatChanged(chatId)

        val envelopeFrame = try {
            val body = MessageBody(
                kind = KIND_TEXT,
                chatId = identity.userId, // wire convention: sender's own userId -- see MeshService KDoc
                lamport = lamport,
                timestamp = timestamp,
                content = payload,
            )
            encodeEnvelopeFrame(sealMessage(identity, contact.agreePk, encodeMessageBody(body)))
        } catch (e: CoreException) {
            Log.w(TAG, "sendText: failed to seal message for ${contact.name}: ${e.message}")
            null
        }

        if (envelopeFrame != null && !MeshRouter.sendToUserId(contact.userId, envelopeFrame)) {
            Log.i(TAG, "sendText: ${contact.name} not currently connected; message stays local until next digest sync")
        }
    }
}
