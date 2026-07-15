package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.chat.nextAuthoredLamport
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.buildOutboundAuthoredEnvelope
import com.cruisemesh.app.mesh.encodeOutboundEnvelopeFrame
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.FriendDirectoryContent
import uniffi.cruisemesh_core.FriendDirectoryEntry
import uniffi.cruisemesh_core.FriendSuggestion
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.IntroducedFriendRequest
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.SuggestedFriendCard
import uniffi.cruisemesh_core.createIntroductionTicket
import uniffi.cruisemesh_core.encodeFriendDirectoryContent
import uniffi.cruisemesh_core.encodeIntroducedFriendRequest
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.makeFriendCard

private const val TAG = "FriendDirectory"
private const val KIND_FRIEND_DIRECTORY: UByte = 6u
private const val KIND_INTRODUCED_FRIEND_REQUEST: UByte = 7u
private const val RECEIPT_TYPE_DELIVERED: UByte = 1u
private const val RECEIPT_TYPE_READ: UByte = 2u
private const val TICKET_LIFETIME_MS = 30L * 24 * 60 * 60 * 1000

/** Publishes personalized, replaceable contact suggestions to accepted contacts. */
object FriendDirectorySender {
    fun queueToAllContacts(
        context: Context,
        store: MessageStore,
        identity: Identity,
    ) {
        val recipients = store.listContacts()
        val revision = FriendsOfFriendsStore.nextDirectoryRevision(context)
        val enabled = FriendsOfFriendsStore.isEnabled(context)
        val now = System.currentTimeMillis()
        for (recipient in recipients) {
            val entries = if (enabled) {
                recipients.asSequence()
                    .filterNot { it.userId.contentEquals(recipient.userId) }
                    .mapNotNull { candidate ->
                        val policy = store.getContactDiscoveryPolicy(candidate.userId)
                            ?: return@mapNotNull null
                        if (policy.protocolVersion < 1u || !policy.enabled) return@mapNotNull null
                        val ticket = createIntroductionTicket(
                            identity,
                            candidate.userId,
                            recipient.userId,
                            policy.revision,
                            now,
                            now + TICKET_LIFETIME_MS,
                            generateMsgId(),
                        )
                        FriendDirectoryEntry(
                            candidate = SuggestedFriendCard(
                                name = candidate.name,
                                userId = candidate.userId,
                                signPk = candidate.signPk,
                                agreePk = candidate.agreePk,
                            ),
                            candidatePolicyRevision = policy.revision,
                            ticket = ticket,
                        )
                    }
                    .take(64)
                    .toList()
            } else {
                emptyList()
            }
            queueDirectory(store, identity, recipient, revision, entries, now)
        }
    }

    private fun queueDirectory(
        store: MessageStore,
        identity: Identity,
        recipient: Contact,
        revision: ULong,
        entries: List<FriendDirectoryEntry>,
        timestamp: Long,
    ) {
        val lamport = nextLamport(store, identity, recipient.userId)
        val message = StoredMessage(
            chatId = recipient.userId,
            senderUserId = identity.userId,
            lamport = lamport,
            timestamp = timestamp,
            kind = KIND_FRIEND_DIRECTORY,
            payload = encodeFriendDirectoryContent(FriendDirectoryContent(1u, revision, entries)),
        )
        queue(store, identity, recipient, message)
    }

    fun requestSuggestedFriend(
        context: Context,
        store: MessageStore,
        identity: Identity,
        suggestion: FriendSuggestion,
    ): Boolean {
        if (!FriendsOfFriendsStore.isEnabled(context)) return false
        val candidate = Contact(
            userId = suggestion.candidate.userId,
            name = suggestion.candidate.name,
            signPk = suggestion.candidate.signPk,
            agreePk = suggestion.candidate.agreePk,
            relayUrl = null,
            relayToken = null,
        )
        val relay = RelayConfigStore.load(context)
        val ownCard = makeFriendCard(
            ProfileStore.loadDisplayName(context),
            identity,
            relay?.relayUrl,
            relay?.relayToken,
        )
        val timestamp = System.currentTimeMillis()
        val message = StoredMessage(
            chatId = candidate.userId,
            senderUserId = identity.userId,
            lamport = nextLamport(store, identity, candidate.userId),
            timestamp = timestamp,
            kind = KIND_INTRODUCED_FRIEND_REQUEST,
            payload = encodeIntroducedFriendRequest(
                IntroducedFriendRequest(1u, ownCard, suggestion.ticket),
            ),
        )
        val queued = queue(store, identity, candidate, message)
        if (queued) store.setFriendSuggestionState(candidate.userId, 1u)
        return queued
    }

    private fun nextLamport(store: MessageStore, identity: Identity, chatId: ByteArray): ULong =
        nextAuthoredLamport(
            ownContiguous = store.highestContiguousLamport(chatId, identity.userId),
            ackedDelivered = store.receiptThrough(chatId, identity.userId, RECEIPT_TYPE_DELIVERED),
            ackedRead = store.receiptThrough(chatId, identity.userId, RECEIPT_TYPE_READ),
        )

    private fun queue(
        store: MessageStore,
        identity: Identity,
        recipient: Contact,
        message: StoredMessage,
    ): Boolean {
        val outbound = buildOutboundAuthoredEnvelope(identity, recipient, message) ?: return false
        store.insertOutgoingMessage(message, outbound, message.timestamp)
        RelaySyncEvents.requestSync()
        val frame = encodeOutboundEnvelopeFrame(outbound)
        if (!MeshRouter.sendToUserId(recipient.userId, frame)) {
            val muled = MeshRouter.relayToAll(frame)
            Log.i(TAG, "Queued hidden friend data for ${UserIdHex.encode(recipient.userId)}; sprayed to $muled mule(s)")
        }
        return true
    }
}
