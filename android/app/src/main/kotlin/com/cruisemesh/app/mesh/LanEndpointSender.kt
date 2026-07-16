package com.cruisemesh.app.mesh

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.chat.nextAuthoredLamport
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.LanEndpointContent
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.encodeLanEndpointContent

private const val TAG = "LanEndpointSender"
private const val KIND_LAN_ENDPOINT_HINT: UByte = 8u
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u
private const val HINT_LIFETIME_MS = 15 * 60 * 1_000L

internal object LanEndpointSender {
    fun queueToAllCapableContacts(
        context: Context,
        store: MessageStore,
        identity: Identity,
        hint: Frame.LanEndpoint,
        networkId: String?,
    ) {
        if (networkId == null) return
        for (contact in store.listContacts()) {
            if (!LanCapabilityStore.isSupported(context, contact.userId)) continue
            queueToContact(context, store, identity, contact, hint, networkId)
        }
    }

    fun queueToContact(
        context: Context,
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        hint: Frame.LanEndpoint,
        networkId: String,
    ) {
        if (
            !LanCapabilityStore.shouldSendEndpoint(
                context,
                contact.userId,
                networkId,
                hint.host,
                hint.port.toInt(),
                hint.instanceToken,
            )
        ) {
            return
        }
        val lamport = nextAuthoredLamport(
            ownContiguous = store.highestContiguousLamport(contact.userId, identity.userId),
            ackedDelivered = store.receiptThrough(
                contact.userId,
                identity.userId,
                RECEIPT_TYPE_DELIVERED,
            ),
            ackedRead = store.receiptThrough(
                contact.userId,
                identity.userId,
                RECEIPT_TYPE_READ,
            ),
        )
        val timestamp = System.currentTimeMillis()
        val payload = try {
            encodeLanEndpointContent(
                LanEndpointContent(
                    instanceToken = hint.instanceToken,
                    networkId = networkId.toByteArray(Charsets.UTF_8),
                    host = hint.host,
                    port = hint.port,
                    expiresAtMs = timestamp + HINT_LIFETIME_MS,
                ),
            )
        } catch (error: Exception) {
            Log.w(TAG, "Unable to encode sealed LAN endpoint hint", error)
            return
        }
        val message = StoredMessage(
            chatId = contact.userId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_LAN_ENDPOINT_HINT,
            payload = payload,
        )
        val outbound = buildOutboundAuthoredEnvelope(identity, contact, message) ?: return
        store.insertOutgoingMessage(message, outbound, timestamp)
        RelaySyncEvents.requestSync()
        if (!MeshRouter.sendToUserId(contact.userId, encodeOutboundEnvelopeFrame(outbound))) {
            Log.i(
                TAG,
                "Queued sealed LAN endpoint for ${UserIdHex.encode(contact.userId)}",
            )
        }
    }
}
