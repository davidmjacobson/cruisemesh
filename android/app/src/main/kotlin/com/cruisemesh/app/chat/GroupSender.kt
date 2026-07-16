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
import com.cruisemesh.app.media.KIND_REACTION
import uniffi.cruisemesh_core.createGroup
import uniffi.cruisemesh_core.encodeGroupInviteContent

private const val TAG = "GroupSender"

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: UByte = 1u

/** Cumulative-receipt type bytes (DESIGN.md §7.2), mirroring the private
 * copies already established in MainActivity.kt / ChatScreen.kt / MeshService.kt. */
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u

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
    fun sendText(group: Group, text: String, replyToMsgId: ByteArray? = null): SendResult {
        val payload = text.toByteArray(Charsets.UTF_8)
        if (payload.isEmpty()) return SendResult.FAILED

        return enqueueGroupMessage(group, KIND_TEXT, payload, "sendText", replyToMsgId)
    }

    /** Sends or clears this user's emoji reaction to [target] in [group]. */
    fun sendReaction(group: Group, target: MessageTarget, emoji: String): SendResult {
        val payload = try {
            ReactionPayload(target, emoji).encode()
        } catch (e: Exception) {
            Log.e(TAG, "sendReaction: could not encode group reaction", e)
            return SendResult.FAILED
        }
        return enqueueGroupMessage(group, KIND_REACTION, payload, "sendReaction")
    }

    private fun enqueueGroupMessage(
        group: Group,
        kind: UByte,
        payload: ByteArray,
        logLabel: String,
        replyToMsgId: ByteArray? = null,
    ): SendResult {
        val queued = try {
            val chatId = group.id
            // Same ratchet-past-acked-receipts logic as 1:1 sends (MeshSender.kt's
            // nextAuthoredLamport): guards against a deleted-and-recreated group
            // chat wiping our own history while a member still holds our old
            // stream. Group wire receipts don't exist yet, so receiptThrough
            // always returns 0 here and maxOf degrades to plain
            // highestContiguousLamport(...) + 1 -- this is forward-compatible
            // wiring for whenever per-member group receipts land.
            val lamport = nextAuthoredLamport(
                ownContiguous = store.highestContiguousLamport(chatId, identity.userId),
                ackedDelivered = store.receiptThrough(chatId, identity.userId, RECEIPT_TYPE_DELIVERED),
                ackedRead = store.receiptThrough(chatId, identity.userId, RECEIPT_TYPE_READ),
            )
            val timestamp = System.currentTimeMillis()
            val message = StoredMessage(
                chatId = chatId,
                senderUserId = identity.userId,
                lamport = lamport,
                timestamp = timestamp,
                kind = kind,
                payload = payload,
            )
            val outbound = buildOutboundGroupEnvelope(identity, group, message, replyToMsgId)
            if (outbound == null) {
                Log.e(TAG, "$logLabel: could not build the durable group envelope for ${group.name}")
                return SendResult.FAILED
            }
            if (replyToMsgId == null) {
                store.insertOutgoingMessage(message, outbound, timestamp)
            } else {
                store.insertOutgoingReply(message, outbound, replyToMsgId, timestamp)
            }
            chatId to outbound
        } catch (e: Exception) {
            Log.e(TAG, "$logLabel: group message was not stored for ${group.name}", e)
            return SendResult.FAILED
        }

        val (chatId, outbound) = queued
        try {
            ChatEvents.notifyChatChanged(chatId)
            RelaySyncEvents.requestSync()
            val frame = encodeOutboundEnvelopeFrame(outbound)
            val fanout = MeshRouter.relayToAll(frame)
            if (fanout == 0) {
                Log.i(TAG, "$logLabel: no live links; group message stays local for carry/digest/relay")
            } else {
                Log.i(TAG, "$logLabel: flooded group message to $fanout link(s)")
            }
        } catch (e: Exception) {
            Log.w(TAG, "$logLabel: stored locally; immediate group delivery will retry", e)
        }
        return SendResult.STORED
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
        // See sendText's comment: same ratchet, and group receipts are still
        // unwired so this degrades to highestContiguousLamport(...) + 1 today.
        val lamport = nextAuthoredLamport(
            ownContiguous = store.highestContiguousLamport(group.id, identity.userId),
            ackedDelivered = store.receiptThrough(group.id, identity.userId, RECEIPT_TYPE_DELIVERED),
            ackedRead = store.receiptThrough(group.id, identity.userId, RECEIPT_TYPE_READ),
        )
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
