package com.cruisemesh.app.chat

import android.util.Log
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import com.cruisemesh.app.media.KIND_GROUP_INVITE
import com.cruisemesh.app.media.KIND_REACTION
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import uniffi.cruisemesh_core.createGroup

private const val TAG = "GroupSender"

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: UByte = 1u

/** Cumulative-receipt type bytes (DESIGN.md §7.2), mirroring the private
 * copies already established in MainActivity.kt / ChatScreen.kt / MeshService.kt. */

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

    fun sendAttachment(
        group: Group,
        attachment: AttachmentPayload,
        replyToMsgId: ByteArray? = null,
    ): SendResult {
        if (attachment.blob.size > AttachmentPayload.MAX_BLOB_BYTES) return SendResult.FAILED
        val payload = try {
            attachment.encode()
        } catch (e: Exception) {
            Log.e(TAG, "sendAttachment: could not encode group attachment", e)
            return SendResult.FAILED
        }
        return enqueueGroupMessage(
            group,
            KIND_ATTACHMENT_MANIFEST,
            payload,
            "sendAttachment",
            replyToMsgId,
        )
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

    /** Rename locally and queue a convergent hidden update for every member. */
    fun renameGroup(group: Group, name: String): Group? {
        val trimmed = name.trim()
        if (trimmed.isEmpty() || trimmed == group.name) return null
        val result = try {
            store.authorGroupMetadataUpdate(
                identity,
                group,
                trimmed,
                group.memberUserIds,
                System.currentTimeMillis(),
            )
        } catch (e: Exception) {
            Log.e(TAG, "renameGroup: metadata update was not stored", e)
            return null
        }
        publishGroupFrame("renameGroup", result.authored)
        ChatEvents.notifyChatChanged(group.id)
        return result.group
    }

    /** Add accepted contacts without rotating/removing the existing group key. */
    fun addMembers(group: Group, additions: List<Contact>): Group? {
        val newMembers = additions.filterNot { addition ->
            group.memberUserIds.any { it.contentEquals(addition.userId) }
        }.distinctBy { it.userId.toList() }
        if (newMembers.isEmpty()) return null
        val allIds = group.memberUserIds + newMembers.map { it.userId }
        val result = try {
            store.authorGroupMetadataUpdate(
                identity,
                group,
                group.name,
                allIds,
                System.currentTimeMillis(),
            )
        } catch (e: Exception) {
            Log.e(TAG, "addMembers: metadata update was not stored", e)
            return null
        }

        // Give new members the key before flooding the group-sealed metadata
        // update; both remain durably queued if nobody is live right now.
        queueInvites(result.group, newMembers)
        publishGroupFrame("addMembers", result.authored)
        ChatEvents.notifyChatChanged(group.id)
        return result.group
    }

    private fun enqueueGroupMessage(
        group: Group,
        kind: UByte,
        payload: ByteArray,
        logLabel: String,
        replyToMsgId: ByteArray? = null,
    ): SendResult {
        val queued = try {
            val timestamp = System.currentTimeMillis()
            val authored = store.authorGroupMessage(identity, group, kind, payload, replyToMsgId, timestamp)
            authored.message.chatId to authored
        } catch (e: Exception) {
            Log.e(TAG, "$logLabel: group message was not stored for ${group.name}", e)
            return SendResult.FAILED
        }

        val (chatId, authored) = queued
        ChatEvents.notifyChatChanged(chatId)
        publishGroupFrame(logLabel, authored)
        return SendResult.STORED
    }

    private fun publishGroupFrame(logLabel: String, authored: uniffi.cruisemesh_core.AuthoredEnvelope) {
        try {
            RelaySyncEvents.requestSync()
            val fanout = MeshRouter.relayToAll(authored.frame)
            if (fanout == 0) {
                Log.i(TAG, "$logLabel: no live links; group message stays local for carry/digest/relay")
            } else {
                Log.i(TAG, "$logLabel: flooded group message to $fanout link(s)")
            }
        } catch (e: Exception) {
            Log.w(TAG, "$logLabel: stored locally; immediate group delivery will retry", e)
        }
    }

    /**
     * One pairwise-sealed invite per other member. Local history stores a
     * single `kind=4` row under `chat_id = group.id`; the outbound queue holds
     * N sealed envelopes keyed by recipient (see core store docs).
     */
    private fun queueInvites(group: Group, members: List<Contact>) {
        val authored = try {
            store.queueGroupInvites(identity, group, members, System.currentTimeMillis())
        } catch (e: Exception) {
            Log.w(TAG, "queueGroupInvites failed: ${e.message}")
            return
        }
        for (invite in authored) {
            if (!MeshRouter.sendToUserId(invite.envelope.recipientUserId, invite.frame)) {
                Log.i(
                    TAG,
                    "Queued group invite; peer not currently connected",
                )
            }
        }
        if (authored.isNotEmpty()) {
            ChatEvents.notifyChatChanged(group.id)
        }
    }
}
