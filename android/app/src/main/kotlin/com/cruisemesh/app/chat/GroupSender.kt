package com.cruisemesh.app.chat

import android.util.Log
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.buildOutboundAuthoredEnvelope
import com.cruisemesh.app.mesh.buildOutboundGroupEnvelope
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import com.cruisemesh.app.media.KIND_GROUP_INVITE
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.encodeGroupInviteContent

private const val TAG = "GroupSender"

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: UByte = 1u

/**
 * Creates groups, fans out pairwise `kind=4` invites, and authors group text
 * (DESIGN.md §6.5). Group text is sealed once with the shared group key and
 * flooded; invites are one pairwise-sealed envelope per other member so each
 * recipient can import the group key under their existing 1:1 crypto.
 */
class GroupSender(
    private val store: MessageStore,
    private val identity: Identity,
) {
    /**
     * Creates a group containing [identity] plus [members], persists it, and
     * queues a pairwise invite to every other member. Returns the created
     * [Group], or null if creation failed / no members selected.
     */
    fun createAndInvite(name: String, members: List<Contact>): Group? {
        val trimmed = name.trim()
        if (trimmed.isEmpty()) {
            Log.w(TAG, "Refusing to create a group with an empty name")
            return null
        }
        if (members.isEmpty()) {
            Log.w(TAG, "Refusing to create a group with no other members")
            return null
        }

        val memberIds = ArrayList<ByteArray>(members.size + 1)
        memberIds.add(identity.userId)
        for (member in members) {
            if (!memberIds.any { it.contentEquals(member.userId) }) {
                memberIds.add(member.userId)
            }
        }

        val group = try {
            createGroup(trimmed, memberIds)
        } catch (e: Exception) {
            Log.w(TAG, "createGroup failed: ${e.message}")
            return null
        }

        store.upsertGroup(group)
        queueInvites(group, members)
        ChatEvents.notifyChatChanged(group.id)
        RelaySyncEvents.requestSync()
        return group
    }

    /** Sends [text] into [group]'s chat stream, sealed with the group key. */
    fun sendText(group: Group, text: String) {
        val payload = text.toByteArray(Charsets.UTF_8)
        if (payload.isEmpty()) return

        val chatId = group.id
        val lamport = store.highestContiguousLamport(chatId, identity.userId) + 1uL
        val timestamp = System.currentTimeMillis()
        val message = StoredMessage(
            chatId = chatId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_TEXT,
            payload = payload,
        )
        val outbound = buildOutboundGroupEnvelope(identity, group, message) ?: return
        store.insertOutgoingMessage(message, outbound, timestamp)
        ChatEvents.notifyChatChanged(chatId)
        RelaySyncEvents.requestSync()

        val frame = encodeOutboundEnvelopeFrame(outbound)
        val fanout = MeshRouter.relayToAll(frame)
        if (fanout == 0) {
            Log.i(TAG, "sendText: no live links; group message stays local for carry/digest/relay")
        } else {
            Log.i(TAG, "sendText: flooded group message to $fanout link(s)")
        }
    }

    /**
     * One pairwise-sealed invite per other member. Local history stores a
     * single `kind=4` row under `chat_id = group.id`; the outbound queue holds
     * N sealed envelopes keyed by recipient (see core store docs).
     */
    private fun queueInvites(group: Group, members: List<Contact>) {
        val inviteContent = try {
            encodeGroupInviteContent(group)
        } catch (e: Exception) {
            Log.w(TAG, "encodeGroupInviteContent failed: ${e.message}")
            return
        }
        val lamport = store.highestContiguousLamport(group.id, identity.userId) + 1uL
        val timestamp = System.currentTimeMillis()
        val message = StoredMessage(
            chatId = group.id,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_GROUP_INVITE,
            payload = inviteContent,
        )

        var anyQueued = false
        for (member in members) {
            if (member.userId.contentEquals(identity.userId)) continue
            val outbound = buildOutboundAuthoredEnvelope(identity, member, message) ?: continue
            store.insertOutgoingMessage(message, outbound, timestamp)
            anyQueued = true
            if (!MeshRouter.sendToUserId(member.userId, encodeOutboundEnvelopeFrame(outbound))) {
                Log.i(
                    TAG,
                    "Queued group invite for ${member.name}; peer not currently connected",
                )
            }
        }
        if (anyQueued) {
            ChatEvents.notifyChatChanged(group.id)
        }
    }
}
