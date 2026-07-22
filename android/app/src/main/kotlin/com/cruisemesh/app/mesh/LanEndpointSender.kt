package com.cruisemesh.app.mesh

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.LanEndpointContent
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.encodeLanEndpointContent

private const val TAG = "LanEndpointSender"
// KIND_LAN_ENDPOINT_HINT comes from MeshWireConstants.kt (FA15).
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
        val authored = store.authorPairwiseMessage(
            identity,
            contact,
            KIND_LAN_ENDPOINT_HINT,
            payload,
            null,
            timestamp,
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        RelaySyncEvents.requestSync()
        if (!MeshRouter.sendToUserId(contact.userId, authored.frame)) {
            Log.i(
                TAG,
                "Queued sealed LAN endpoint for ${UserIdHex.encode(contact.userId)}",
            )
        }
    }
}
