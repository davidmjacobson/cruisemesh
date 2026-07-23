package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.GossipState
import com.cruisemesh.app.mesh.RelaySyncEvents
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.ProfileSyncContent
import uniffi.cruisemesh_core.encodeProfileSyncContent

private const val TAG = "ProfileSyncSender"
private const val KIND_PROFILE_SYNC: UByte = 5u

object ProfileSyncSender {

    fun queueToAllContacts(
        context: Context,
        store: MessageStore,
        identity: Identity,
        epoch: Long,
    ) {
        val avatar = ProfilePhotoStore.loadWireAvatarBytes(context)
        val name = ProfileStore.loadDisplayName(context)
        val friendsOfFriendsEnabled = FriendsOfFriendsStore.isEnabled(context)
        val friendsOfFriendsRevision = FriendsOfFriendsStore.revision(context)
        // Blocked contacts get nothing from us — not even profile updates.
        val blocked = store.listBlockedUsers()
        for (contact in store.listContacts()) {
            if (blocked.any { it.contentEquals(contact.userId) }) continue
            queueToContact(
                store,
                identity,
                contact,
                epoch,
                name,
                avatar,
                friendsOfFriendsEnabled,
                friendsOfFriendsRevision,
            )
        }
    }

    fun queueToContact(
        context: Context,
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        epoch: Long,
    ) {
        queueToContact(
            store = store,
            identity = identity,
            contact = contact,
            epoch = epoch,
            name = ProfileStore.loadDisplayName(context),
            avatar = ProfilePhotoStore.loadWireAvatarBytes(context),
            friendsOfFriendsEnabled = FriendsOfFriendsStore.isEnabled(context),
            friendsOfFriendsRevision = FriendsOfFriendsStore.revision(context),
        )
    }

    private fun queueToContact(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        epoch: Long,
        name: String,
        avatar: ByteArray,
        friendsOfFriendsEnabled: Boolean,
        friendsOfFriendsRevision: ULong,
    ) {
        val timestamp = System.currentTimeMillis()
        val payload = try {
            encodeProfileSyncContent(
                ProfileSyncContent(
                    avatarEpoch = epoch,
                    name = name,
                    avatar = avatar,
                    friendsOfFriendsVersion = 1u,
                    friendsOfFriendsEnabled = friendsOfFriendsEnabled,
                    friendsOfFriendsRevision = friendsOfFriendsRevision,
                ),
            )
        } catch (e: Exception) {
            Log.w(TAG, "Skipping invalid profile sync payload", e)
            return
        }
        val authored = store.authorPairwiseMessage(
            identity,
            contact,
            KIND_PROFILE_SYNC,
            payload,
            null,
            timestamp,
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        RelaySyncEvents.requestSync()

        if (!MeshRouter.sendToUserId(contact.userId, authored.frame)) {
            Log.i(
                TAG,
                "Queued profile sync for ${UserIdHex.encode(contact.userId)}; peer not currently connected",
            )
        }
    }
}
