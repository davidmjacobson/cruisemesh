package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.makeFriendCard

private const val TAG = "FriendRequestSender"
private const val KIND_FRIEND_REQUEST: UByte = 3u

/** Cumulative-receipt type bytes (DESIGN.md §7.2), mirroring the private
 * copies already established in MainActivity.kt / ChatScreen.kt / MeshService.kt. */

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
        val timestamp = System.currentTimeMillis()
        val authored = try {
            store.authorFriendRequest(identity, contact, cardJson, timestamp)
        } catch (e: Exception) {
            return FriendRequestDelivery(reachedDirectly = false, lamport = 0uL)
        }
        RelaySyncEvents.requestSync()

        val reachedDirectly = MeshRouter.sendToUserId(contact.userId, authored.frame)
        if (!reachedDirectly) {
            val muled = MeshRouter.relayToAll(authored.frame)
            Log.i(
                TAG,
                "Queued friend request for ${UserIdHex.encode(contact.userId)}; " +
                    "peer not currently connected, sprayed to $muled mule link(s)",
            )
        }
        return FriendRequestDelivery(reachedDirectly = reachedDirectly, lamport = authored.message.lamport)
    }
}
