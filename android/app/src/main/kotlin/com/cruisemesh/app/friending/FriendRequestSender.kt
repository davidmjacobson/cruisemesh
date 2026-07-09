package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
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
    ) {
        val relay = RelayConfigStore.load(context)
        val cardJson = makeFriendCard(
            ProfileStore.loadDisplayName(context),
            identity,
            relay?.relayUrl,
            relay?.relayToken,
        )
        val lamport = store.highestContiguousLamport(contact.userId, identity.userId) + 1uL
        val timestamp = System.currentTimeMillis()
        val message = StoredMessage(
            chatId = contact.userId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_FRIEND_REQUEST,
            payload = cardJson.toByteArray(Charsets.UTF_8),
        )
        val outbound = buildOutboundAuthoredEnvelope(identity, contact, message) ?: return
        store.insertOutgoingMessage(message, outbound, timestamp)
        RelaySyncEvents.requestSync()

        if (!MeshRouter.sendToUserId(contact.userId, encodeOutboundEnvelopeFrame(outbound))) {
            Log.i(
                TAG,
                "Queued friend request for ${UserIdHex.encode(contact.userId)}; peer not currently connected",
            )
        }
    }
}
