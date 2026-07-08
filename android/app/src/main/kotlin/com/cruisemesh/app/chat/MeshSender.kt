package com.cruisemesh.app.chat

import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage

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
 * Placeholder [MeshSender]: persists the outgoing message straight into the
 * local [MessageStore] and does **not** transmit it anywhere. This stands in
 * for the real pipeline -- sealing the plaintext body (DESIGN.md §6.3) and
 * handing it to `MeshService` for BLE delivery (DESIGN.md §5.2) -- which is
 * the next milestone's work. Because the UI only ever talks to the
 * [MeshSender] interface, swapping this stub out for the real implementation
 * later requires no UI changes.
 *
 * 1:1 chats use the peer's UserID as `chat_id` (DESIGN.md §7.1), so the
 * outgoing message is stored under `contact.userId` even though `ownUserId`
 * is the sender. Computing `lamport` as `highestContiguousLamport(...) + 1`
 * is safe here specifically because our own outgoing stream is authored
 * locally and therefore has no gaps (DESIGN.md §7.1, §7.3) -- a received
 * stream would need the real gap-aware sync logic instead.
 */
class LocalEchoSender(
    private val store: MessageStore,
    private val ownUserId: ByteArray,
) : MeshSender {
    override fun sendText(contact: Contact, text: String) {
        val chatId = contact.userId
        val lamport = store.highestContiguousLamport(chatId, ownUserId) + 1UL
        store.insertMessage(
            StoredMessage(
                chatId = chatId,
                senderUserId = ownUserId,
                lamport = lamport,
                timestamp = System.currentTimeMillis(),
                kind = KIND_TEXT,
                payload = text.toByteArray(Charsets.UTF_8),
            ),
        )
    }
}
