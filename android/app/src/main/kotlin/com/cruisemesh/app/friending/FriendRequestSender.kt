package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.chat.nextAuthoredLamport
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.buildOutboundAuthoredEnvelope
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.makeFriendCard

private const val TAG = "FriendRequestSender"
private const val KIND_FRIEND_REQUEST: UByte = 3u

/** Cumulative-receipt type bytes (DESIGN.md §7.2), mirroring the private
 * copies already established in MainActivity.kt / ChatScreen.kt / MeshService.kt. */
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u

/**
 * Queues the mutual-friending follow-up from DESIGN.md §6.2: once we scan a
 * peer's QR card and import them locally, send our own signed friend card
 * back as a hidden `kind=3` chat-stream message so they can auto-import us.
 */
object FriendRequestSender {

    fun queueForScannedContact(
        context: Context,
        store: MessageStore,
        identity: Identity,
        contact: Contact,
    ): FriendRequestDelivery {
        val relay = RelayConfigStore.load(context)
        val cardJson = makeFriendCard(
            ProfileStore.loadDisplayName(context),
            identity,
            relay?.relayUrl,
            relay?.relayToken,
        )
        // Same ratchet-past-acked-receipts logic as 1:1 text sends
        // (MeshSender.kt's nextAuthoredLamport): a friend request also lands
        // in the 1:1 chat's lamport stream, so it needs the same guard
        // against reissuing a lamport the peer already holds from before a
        // chat delete + re-add wiped our own history.
        val lamport = nextAuthoredLamport(
            ownContiguous = store.highestContiguousLamport(contact.userId, identity.userId),
            ackedDelivered = store.receiptThrough(contact.userId, identity.userId, RECEIPT_TYPE_DELIVERED),
            ackedRead = store.receiptThrough(contact.userId, identity.userId, RECEIPT_TYPE_READ),
        )
        val timestamp = System.currentTimeMillis()
        val message = StoredMessage(
            chatId = contact.userId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_FRIEND_REQUEST,
            payload = cardJson.toByteArray(Charsets.UTF_8),
        )
        val outbound = buildOutboundAuthoredEnvelope(identity, contact, message)
            ?: return FriendRequestDelivery(reachedDirectly = false, lamport = lamport)
        store.insertOutgoingMessage(message, outbound, timestamp)
        RelaySyncEvents.requestSync()

        val frame = encodeOutboundEnvelopeFrame(outbound)
        val reachedDirectly = MeshRouter.sendToUserId(contact.userId, frame)
        if (!reachedDirectly) {
            val muled = MeshRouter.relayToAll(frame)
            Log.i(
                TAG,
                "Queued friend request for ${UserIdHex.encode(contact.userId)}; " +
                    "peer not currently connected, sprayed to $muled mule link(s)",
            )
        }
        return FriendRequestDelivery(reachedDirectly = reachedDirectly, lamport = lamport)
    }
}
