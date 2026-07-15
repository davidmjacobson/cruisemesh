package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.chat.nextAuthoredLamport
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.buildOutboundAuthoredEnvelope
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.ProfileSyncContent
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.encodeProfileSyncContent

private const val TAG = "ProfileSyncSender"
private const val KIND_PROFILE_SYNC: UByte = 5u
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u

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
        for (contact in store.listContacts()) {
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
        val lamport = nextAuthoredLamport(
            ownContiguous = store.highestContiguousLamport(contact.userId, identity.userId),
            ackedDelivered = store.receiptThrough(contact.userId, identity.userId, RECEIPT_TYPE_DELIVERED),
            ackedRead = store.receiptThrough(contact.userId, identity.userId, RECEIPT_TYPE_READ),
        )
        val timestamp = System.currentTimeMillis()
        val payload = encodeProfileSyncContent(
            ProfileSyncContent(
                avatarEpoch = epoch,
                name = name,
                avatar = avatar,
                friendsOfFriendsVersion = 1u,
                friendsOfFriendsEnabled = friendsOfFriendsEnabled,
                friendsOfFriendsRevision = friendsOfFriendsRevision,
            ),
        )
        val message = StoredMessage(
            chatId = contact.userId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_PROFILE_SYNC,
            payload = payload,
        )
        val outbound = buildOutboundAuthoredEnvelope(identity, contact, message) ?: return
        store.insertOutgoingMessage(message, outbound, timestamp)
        RelaySyncEvents.requestSync()

        if (!MeshRouter.sendToUserId(contact.userId, encodeOutboundEnvelopeFrame(outbound))) {
            Log.i(
                TAG,
                "Queued profile sync for ${UserIdHex.encode(contact.userId)}; peer not currently connected",
            )
        }
    }
}
