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
import uniffi.cruisemesh_core.SharedFriendCard
import uniffi.cruisemesh_core.makeFriendCard
import uniffi.cruisemesh_core.makeSharedFriendRequestPayload

private const val TAG = "FriendRequestSender"

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
    ): FriendRequestDelivery = queue(context, store, identity, contact, shared = null)

    /**
     * The same mutual `kind=3`, carrying the shared card it came from as the
     * optional tail (specs/share-contact.md). The tail is what makes the other
     * phone hold the request for confirmation instead of auto-importing, so it
     * must ride on exactly the request a shared-card import sends back.
     */
    fun queueForSharedCard(
        context: Context,
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        shared: SharedFriendCard,
    ): FriendRequestDelivery = queue(context, store, identity, contact, shared)

    private fun queue(
        context: Context,
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        shared: SharedFriendCard?,
    ): FriendRequestDelivery {
        val relay = RelayConfigStore.load(context)
        val payload = try {
            val cardJson = makeFriendCard(
                ProfileStore.loadDisplayName(context),
                identity,
                relay?.relayUrl,
                relay?.relayToken,
            )
            if (shared == null) cardJson else makeSharedFriendRequestPayload(cardJson, shared)
        } catch (e: Exception) {
            Log.w(TAG, "Skipping invalid friend card", e)
            return FriendRequestDelivery(reachedDirectly = false, lamport = 0uL)
        }
        val timestamp = System.currentTimeMillis()
        val authored = try {
            store.authorFriendRequest(identity, contact, payload, timestamp)
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
