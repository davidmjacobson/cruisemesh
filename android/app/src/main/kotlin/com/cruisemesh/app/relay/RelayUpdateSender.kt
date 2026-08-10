package com.cruisemesh.app.relay

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.friending.FriendDirectorySender
import com.cruisemesh.app.mesh.GossipState
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.RelayUpdateContent
import uniffi.cruisemesh_core.encodeRelayUpdateContent

private const val TAG = "RelayUpdateSender"
private const val KIND_RELAY_UPDATE: UByte = 9u

/**
 * T23 relay-change propagation, the exact twin of
 * [com.cruisemesh.app.friending.ProfileSyncSender].
 *
 * A friend card is a *snapshot* of the sharer's relay config at the moment it
 * was shared. Buy a Shore Pass, rotate a token, or migrate servers, and every
 * contact keeps posting to the endpoint they were handed — in the field that
 * looked like a contact's phone posting to a long-retired host and collecting
 * `401 unknown family token` roughly ten times a minute, forever, while the
 * messages sat in the outbound queue showing a single tick. Nothing surfaced
 * it and the only repair was re-exchanging cards by hand.
 *
 * The notice carries the **deposit** credential, never the member token: core's
 * [encodeRelayUpdateContent] attenuates whatever it is handed, so this file
 * passes the saved config through unmodified and cannot leak one (CP4).
 */
object RelayUpdateSender {

    /**
     * Fans the current endpoint out to every contact if it has changed since
     * the last successful announcement.
     *
     * Called both at the point of change (immediate) and from app start
     * (catch-up): a save that happened while the mesh was down, or during a
     * backup restore, still reaches contacts on the next launch, and the
     * announced-epoch check makes both paths idempotent.
     */
    fun announceIfChanged(context: Context, store: MessageStore, identity: Identity) {
        val epoch = RelayConfigStore.relayEpoch(context)
        if (epoch <= RelayConfigStore.announcedRelayEpoch(context)) return
        // Our own mailbox moved (new pass, manual edit, restore): everything
        // "already uploaded" was confirmed against the OLD config, so
        // re-offer the whole carry queue once against the new one -- the
        // same wholesale clear core performs when a CONTACT's endpoint moves
        // (apply_contact_relay_update). Runs before this pass's uploads, so
        // the re-offer rides the very sync that detected the change.
        try {
            store.clearCarriedRelayUploadMarkers()
        } catch (e: Exception) {
            Log.w(TAG, "Failed to clear carried-upload markers on endpoint change: ${e.message}")
        }
        queueToAllContacts(context, store, identity, epoch)
        // Which contacts may be introduced to each other is scoped by our own
        // pass (FriendDirectoryScope), so the pass moving changes every
        // snapshot we have ever sent. Re-fan here -- the one place that knows
        // our mailbox changed -- or the old scoping stands until some
        // unrelated contact edit happens to trigger a rebuild.
        FriendDirectorySender.queueToAllContacts(context, store, identity)
        RelayConfigStore.markRelayEpochAnnounced(context, epoch)
    }

    fun queueToAllContacts(
        context: Context,
        store: MessageStore,
        identity: Identity,
        epoch: Long,
    ) {
        queueToAllContacts(store, identity, epoch, RelayConfigStore.load(context))
    }

    /** Pure fan-out seam used by the host-core regression test. */
    internal fun queueToAllContacts(
        store: MessageStore,
        identity: Identity,
        epoch: Long,
        relay: RelayConfig?,
        sendToUser: (ByteArray, ByteArray) -> Boolean = MeshRouter::sendToUserId,
        requestSync: () -> Unit = RelaySyncEvents::requestSync,
    ) {
        // Blocked contacts get nothing from us — not even endpoint changes.
        val blocked = store.listBlockedUsers()
        for (contact in store.listContacts()) {
            if (blocked.any { it.contentEquals(contact.userId) }) continue
            queueToContact(store, identity, contact, epoch, relay, sendToUser, requestSync)
        }
    }

    private fun queueToContact(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        epoch: Long,
        relay: RelayConfig?,
        sendToUser: (ByteArray, ByteArray) -> Boolean,
        requestSync: () -> Unit,
    ) {
        val timestamp = System.currentTimeMillis()
        val payload = try {
            encodeRelayUpdateContent(
                RelayUpdateContent(
                    // Only ever our own UserID: core rejects a notice whose
                    // subject is not the sealing sender, so a third party's
                    // endpoint can never ride along (endpoint privacy).
                    subjectUserId = identity.userId,
                    relayEpoch = epoch,
                    // Empty when the pass lapsed or was removed — an honest
                    // "no internet delivery any more", not a no-op.
                    relayUrl = relay?.relayUrl.orEmpty(),
                    relayToken = relay?.relayToken.orEmpty(),
                ),
            )
        } catch (e: Exception) {
            Log.w(TAG, "Skipping invalid relay update payload", e)
            return
        }
        val authored = try {
            store.authorPairwiseMessage(
                identity,
                contact,
                KIND_RELAY_UPDATE,
                payload,
                null,
                timestamp,
            )
        } catch (e: Exception) {
            Log.w(TAG, "Skipping relay update for ${UserIdHex.encode(contact.userId)}", e)
            return
        }
        GossipState.seenIds.record(authored.envelope.msgId)
        requestSync()

        if (!sendToUser(contact.userId, authored.frame)) {
            Log.i(
                TAG,
                "Queued relay update for ${UserIdHex.encode(contact.userId)}; peer not currently connected",
            )
        }
    }
}
