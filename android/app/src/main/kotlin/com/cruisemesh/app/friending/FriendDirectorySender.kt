package com.cruisemesh.app.friending

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.MeshRouter
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.mesh.GossipState
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.FriendDirectoryContent
import uniffi.cruisemesh_core.FriendDirectoryEntry
import uniffi.cruisemesh_core.FriendSuggestion
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.IntroducedFriendRequest
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.SuggestedFriendCard
import uniffi.cruisemesh_core.createIntroductionTicket
import uniffi.cruisemesh_core.encodeFriendDirectoryContent
import uniffi.cruisemesh_core.encodeIntroducedFriendRequest
import uniffi.cruisemesh_core.generateMsgId
import uniffi.cruisemesh_core.makeFriendCard

private const val TAG = "FriendDirectory"
private const val KIND_FRIEND_DIRECTORY: UByte = 6u
private const val KIND_INTRODUCED_FRIEND_REQUEST: UByte = 7u
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
        queue(
            store,
            identity,
            recipient,
            KIND_FRIEND_DIRECTORY,
            encodeFriendDirectoryContent(FriendDirectoryContent(1u, revision, entries)),
            timestamp,
        )
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
        val queued = queue(
            store,
            identity,
            candidate,
            KIND_INTRODUCED_FRIEND_REQUEST,
            encodeIntroducedFriendRequest(
                IntroducedFriendRequest(1u, ownCard, suggestion.ticket),
            ),
            timestamp,
        )
        if (queued) store.setFriendSuggestionState(candidate.userId, 1u)
        return queued
    }

    private fun queue(
        store: MessageStore,
        identity: Identity,
        recipient: Contact,
        kind: UByte,
        payload: ByteArray,
        timestamp: Long,
    ): Boolean {
        val authored = store.authorPairwiseMessage(identity, recipient, kind, payload, null, timestamp)
        GossipState.seenIds.record(authored.envelope.msgId)
        RelaySyncEvents.requestSync()
        val frame = authored.frame
        if (!MeshRouter.sendToUserId(recipient.userId, frame)) {
            val muled = MeshRouter.relayToAll(frame)
            Log.i(TAG, "Queued hidden friend data for ${UserIdHex.encode(recipient.userId)}; sprayed to $muled mule(s)")
        }
        return true
    }
}
