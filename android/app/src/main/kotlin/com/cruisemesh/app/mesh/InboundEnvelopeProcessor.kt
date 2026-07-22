package com.cruisemesh.app.mesh

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.friending.FriendDirectorySender
import com.cruisemesh.app.friending.FriendImportEvents
import com.cruisemesh.app.friending.FriendRequestSender
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.friending.ProfileSyncSender
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import com.cruisemesh.app.relay.RelayImport
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.ContactDiscoveryPolicy
import uniffi.cruisemesh_core.ContactProvenance
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreInboundGate
import uniffi.cruisemesh_core.DigestEntry
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OpenedMessage
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.applyGroupMetadataUpdate
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.coreInboundGate
import uniffi.cruisemesh_core.coreIsOwnFanoutHint
import uniffi.cruisemesh_core.corePairwiseSenderAuthorized
import uniffi.cruisemesh_core.decodeExtendedMessageBody
import uniffi.cruisemesh_core.decodeFriendDirectoryContent
import uniffi.cruisemesh_core.decodeGroupInviteContent
import uniffi.cruisemesh_core.decodeGroupMetadataUpdate
import uniffi.cruisemesh_core.decodeIntroducedFriendRequest
import uniffi.cruisemesh_core.decodeLanEndpointContent
import uniffi.cruisemesh_core.decodeProfileSyncContent
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.openGroupMessage
import uniffi.cruisemesh_core.openMessage
import uniffi.cruisemesh_core.parseFriendCard
import uniffi.cruisemesh_core.recentHintsFor
import uniffi.cruisemesh_core.verifyIntroductionTicket

// Deliberately MeshService's tag, not this class's name: this code moved here
// verbatim in the FA15 extraction, and field tooling (logcat filters, the
// debug-report scripts) matches on the "MeshService" tag for delivery lines.
private const val TAG = "MeshService"

/** DESIGN.md §5.3: the bounded budget (~5 MB) of *foreign* muled envelopes; family (known-recipient) traffic is exempt. */
private const val FOREIGN_CARRY_BUDGET_BYTES: Long = 5L * 1024 * 1024

/**
 * Muling hook B: bounded per-digest-exchange budget (sealed-byte
 * size) for spraying our own still-undelivered 1:1 outbound envelopes to a
 * non-recipient mule. Same order of magnitude as [FOREIGN_CARRY_BUDGET_BYTES]
 * is generous for storage, but this budget bounds one GATT exchange's worth
 * of traffic, not total storage, so it's much smaller.
 */
private const val OWN_OUTBOUND_SPRAY_BUDGET_BYTES: Long = 256L * 1024

/**
 * Bounded per-digest-exchange budget
 * (sealed-byte size) for spraying our own still-undelivered outgoing receipt
 * envelopes to a mule so it can carry them back toward the original message
 * senders. Receipts are tiny (a fixed cumulative watermark, no message body),
 * so this is far smaller than [OWN_OUTBOUND_SPRAY_BUDGET_BYTES] -- 64 KiB is
 * hundreds of receipts, a backstop against a pathological backlog rather than
 * a normal-case limiter.
 */
private const val OWN_RECEIPT_SPRAY_BUDGET_BYTES: Long = 64L * 1024

/**
 * FA15: the envelope half of what used to be MeshService — everything that
 * happens to a §6.4 envelope after a transport hands it over: the FA5
 * admission claim, the §5.3 gossip gate (dedupe/expiry), open-vs-relay, local
 * delivery per kind, the carry queue, receipt authoring, and digest-time
 * receipt/spray sync. MeshService keeps the transports, link lifecycle, and
 * HELLO/DIGEST session bookkeeping and calls in here; [RelaySyncEngine]
 * hands relay-fetched envelopes to [handleRelayEnvelope] and acks on the
 * returned disposition.
 *
 * ### The wire `chatId` convention (read this before touching frame handling)
 *
 * Locally, a 1:1 chat is always keyed by "the other party's userId" -- see
 * [com.cruisemesh.app.chat.ChatScreen] and [com.cruisemesh.app.chat.RealMeshSender]. A message I
 * send to contact C is stored under `chatId = C.userId`; a message C sends
 * to me is *also* stored under `chatId = C.userId`, because from my side C
 * is always "the other party," regardless of who authored the message.
 *
 * On the wire, though, [MessageBody.chatId] is set by the SENDER to the
 * SENDER's OWN userId, not the recipient's. That looks backwards until you
 * read it from the receiving side: [deliverOpenedEnvelope] below checks
 * `body.chatId == opened.senderUserId`, which only makes sense if wire
 * `chatId` names "whoever sent this frame." That value is also exactly what
 * the receiver needs to store the message under locally (their convention:
 * `chatId` = the other party = the sender). So "wire chatId = sender's own
 * userId" is what makes the sender's and receiver's local conventions line
 * up without either side rewriting anything after the fact. The same
 * convention applies to receipts (see [handleIncomingChatMessage]'s outgoing
 * receipt): a receipt's wire `chatId` is the *receipt sender's* own userId
 * (i.e. mine, when I'm acking someone else's message), for the identical
 * reason. And it applies to DIGEST frames too (DESIGN.md §7.3, see
 * MeshService's `handleHello` outgoing digest and `handleDigest` sanity
 * check): a digest's wire `chatId` is the *digest sender's* own userId, so
 * "does this digest's chatId match what [MeshRouter] learned from this
 * link's HELLO" is exactly the right check for "is this digest about the
 * chat I think it is."
 */
internal class InboundEnvelopeProcessor(
    private val context: Context,
    private val store: MessageStore,
    private val identityProvider: () -> Identity?,
    private val requestRelaySync: (String) -> Unit,
    private val lan: LanHooks,
) {

    /**
     * The few LAN-transport touches the delivery path makes (sealed kind=8
     * endpoint hints, and the eager hint-back on a new friend request), kept
     * behind an interface so this class needs no reference to [LanTransport]
     * or MeshService's endpoint cache. Implemented by MeshService; every
     * method mirrors the null-tolerant behavior of the original
     * `lanTransport?.` call sites.
     */
    interface LanHooks {
        fun sendLanEndpointHintTo(address: String)
        fun connectToLanHint(hint: Frame.LanEndpoint, peerUserId: ByteArray)
        fun saveLanEndpoint(networkId: String?, userId: ByteArray, endpoint: LanManualEndpoint)
        fun currentLanNetworkId(): String?
    }

    /** FA5: atomic per-msg_id admission gate across the four concurrent receive-path threads -- see [processInboundEnvelope]. */
    private val inboundAdmission = InboundEnvelopeAdmission()

    /**
     * DESIGN.md §7.3: receipts go first on peer sync because they're the
     * smallest frames and unblock the most UI. The store persists the latest
     * cumulative delivered/read watermarks we owe [contact], so a receipt that
     * couldn't be sent when it was first observed heals on this reconnect.
     *
     * The digest entry for [contact.userId] is "how far the peer says its own
     * authored stream exists contiguously"; receipts acknowledging beyond that
     * point are capped away as nonsensical. In the ordinary case the cap is a
     * no-op, but it makes the foreign digest entry actively meaningful rather
     * than ignored.
     */
    fun syncReceiptsFirst(
        identity: Identity,
        contact: Contact,
        address: String,
        entries: List<DigestEntry>,
    ) {
        val peerAuthoredThrough = DigestSync.throughLamportForSender(entries, contact.userId)
        if (peerAuthoredThrough == 0uL) return

        val deliveredThrough = minOf(
            store.outgoingReceiptThrough(contact.userId, contact.userId, RECEIPT_TYPE_DELIVERED),
            peerAuthoredThrough,
        )
        if (deliveredThrough > 0uL) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, contact.userId, deliveredThrough)
        }

        val readThrough = minOf(
            store.outgoingReceiptThrough(contact.userId, contact.userId, RECEIPT_TYPE_READ),
            peerAuthoredThrough,
        )
        if (readThrough > 0uL) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, contact.userId, readThrough)
        }
    }

    fun handleRelayEnvelope(
        envelope: RelayFetchedEnvelope,
        identity: Identity,
    ): CoreInboundDisposition {
        Log.i(
            TAG,
            "Handling relay envelope id=${envelope.id} msgId=${UserIdHex.encode(envelope.msgId)} hopTtl=${envelope.hopTtl}",
        )
        return processInboundEnvelope(
            sourceAddress = null,
            envelope = Frame.Envelope(
                msgId = envelope.msgId,
                hopTtl = envelope.hopTtl,
                expiry = envelope.expiryMs,
                recipientHint = envelope.recipientHint,
                sealed = envelope.sealed,
            ),
            identity = identity,
        )
    }

    /**
     * Envelope handling with §5.3 gossip in front of §6.3 delivery.
     *
     * Every inbound `0x02` frame carries the §6.4 public header, so before
     * touching crypto we run the flooding logic DESIGN.md §5.3 calls for:
     *
     * 1. **Dedupe** on `msg_id` via the shared [GossipState.seenIds]. A
     *    `msg_id` we've already handled (on this or any other link, including
     *    one we ourselves authored) is dropped
     *    outright: it was already delivered-or-relayed the first time, and the
     *    mesh's redundant links guarantee we'll see popular frames more than
     *    once. This is the single most important line for not melting the
     *    network with a flood.
     * 2. **Expiry**: a carrier drops an envelope past its `expiry`
     *    (DESIGN.md §5.3) rather than delivering or forwarding it. For
     *    freshly authored direct traffic expiry is a week out so this never
     *    fires; it matters for the old muled traffic a future carry queue
     *    (§5.3) will hold.
     * 3. **Open vs relay**: we try to [openMessage]. A sealed box is anonymous
     *    and addressed to exactly one X25519 key (§6.3), so *opening it means
     *    we are the intended recipient* -- deliver locally and do NOT re-flood
     *    (it's home). Failure means it's foreign traffic just passing through,
     *    so [relayForeignEnvelope] floods it onward with a decremented
     *    `hop_ttl`. (A failure could also be a corrupt/garbage envelope; we
     *    can't tell those apart from "not for us" without the key, and relaying
     *    a few bad frames is cheap and TTL-bounded, so we treat both the same.)
     *
     * Delivery itself (decode body, the `chatId == verified sender` sanity
     * check explained in this class's KDoc, kind dispatch) is unchanged --
     * see [deliverOpenedEnvelope].
     *
     * DTN D4 (seen-set poisoning ordering): [GossipState.seenIds] is checked
     * with the non-mutating [uniffi.cruisemesh_core.SeenIds.contains], never
     * [uniffi.cruisemesh_core.SeenIds.checkAndRecord], and only recorded once
     * this envelope reaches a **terminal handled state** -- consumed,
     * carried, or expired-drop -- at each `return` below. Invariant: an
     * envelope whose durable handling failed must be re-presentable; an
     * envelope that was handled (even by deliberate drop) must be deduped.
     * Before this, `checkAndRecord` ran up front, so a later store failure
     * (e.g. disk-full out of [carryForeignEnvelope]) permanently poisoned the
     * `msg_id` even though it was never actually carried or delivered.
     *
     * This returns a [CoreInboundDisposition] so [RelaySyncEngine]'s mailbox
     * poll (the relay path) can decide whether it's safe to ack the envelope;
     * the BLE path has no such concept (a link frame isn't "acked"), so it
     * just ignores the return value.
     *
     * [sourceAddress] doubles as the source discriminant relay proxy-polling
     * needs: `null` means this envelope came FROM the relay
     * ([handleRelayEnvelope]), non-null means it arrived over a live BLE or
     * authenticated same-LAN link (MeshService's frame dispatch). The two foreign-carry branches below use that to
     * pick [carryRelayEnvelope] (durable, never re-uploaded -- it's already on
     * the relay) vs. the existing [carryForeignEnvelope] (durable-if-family,
     * uploaded to the relay so an internet phone can proxy it onward) for
     * envelopes we can't open ourselves. See [CoreInboundDisposition] for what
     * each return value means to the caller.
     */
    fun processInboundEnvelope(
        sourceAddress: String?,
        envelope: Frame.Envelope,
        identity: Identity,
    ): CoreInboundDisposition {
        val sourceLabel = sourceAddress ?: "relay"
        // FA5: this function runs concurrently on up to four threads (central-
        // GATT binder, peripheral-GATT binder, LanTransport's
        // connectionExecutor, the relay-sync thread) -- see
        // [InboundEnvelopeAdmission]'s KDoc for the full threading model.
        // Claim this msg_id before touching the seen-set or dispatching
        // anything: a rejected claim means another thread is already
        // mid-flight on this exact msg_id right now (e.g. the same message
        // arriving over BLE and LAN at once), so treat it exactly like an
        // ordinary dedupe instead of double-delivering/double-flooding.
        if (!inboundAdmission.tryBegin(envelope.msgId)) {
            return CoreInboundDisposition.SEEN
        }
        // Every return below must go through this so the admission claim
        // above is always released. `terminal = true` also runs
        // GossipState.seenIds.record for this msg_id -- still under
        // [InboundEnvelopeAdmission]'s lock, so no other thread can re-claim
        // this msg_id between the record landing and the claim releasing.
        fun finishAdmission(disposition: CoreInboundDisposition, terminal: Boolean): CoreInboundDisposition {
            inboundAdmission.finish(envelope.msgId, terminal) { GossipState.seenIds.record(envelope.msgId) }
            return disposition
        }

        // DTN D4: a non-mutating check, not checkAndRecord -- see the KDoc
        // above. `record` (via finishAdmission's terminal=true) is only
        // called once handling below actually reaches a terminal state, so a
        // failure partway through leaves this msg_id re-presentable on the
        // next copy instead of poisoned forever.
        when (
            coreInboundGate(
                !GossipState.seenIds.contains(envelope.msgId),
                envelope.hopTtl,
                envelope.expiry,
                System.currentTimeMillis(),
            )
        ) {
            CoreInboundGate.SEEN -> {
                // Already recorded by a prior, non-concurrent copy -- no
                // record() needed, just release this claim.
                return finishAdmission(CoreInboundDisposition.SEEN, terminal = false)
            }
            CoreInboundGate.EXPIRED -> {
                Log.i(TAG, "Dropping expired envelope from $sourceLabel (expiry=${envelope.expiry})")
                // A deliberate drop is still a terminal handled state.
                return finishAdmission(CoreInboundDisposition.EXPIRED, terminal = true)
            }
            CoreInboundGate.REJECTED -> {
                Log.w(TAG, "Dropping envelope with invalid hop or expiry fields from $sourceLabel")
                return finishAdmission(CoreInboundDisposition.REJECTED, terminal = true)
            }
            CoreInboundGate.DISPATCH -> Unit
        }

        val opened = try {
            openMessage(identity, envelope.sealed)
        } catch (e: CoreException) {
            // Pairwise open failed: either foreign 1:1 traffic, or a group
            // envelope sealed with a shared key (DESIGN.md §6.5). Try groups
            // whose recipient_hint matches before treating it as pure mule
            // traffic. Group members keep relaying/carrying so absent members
            // still get a copy (mesh_sim group scenario).
            val groupOpened = tryOpenGroupMessage(envelope.recipientHint, envelope.sealed)
            if (groupOpened != null) {
                val arrival = messageArrival(sourceAddress, envelope.hopTtl, groupOpened.second.senderUserId)
                try {
                    deliverOpenedGroupEnvelope(
                        sourceLabel,
                        groupOpened.first,
                        groupOpened.second,
                        identity,
                        arrival,
                        envelope.msgId,
                    )
                } catch (e: CoreException) {
                    // T4-06: same as the pairwise path below -- a store
                    // failure delivering our own group copy must not unwind
                    // the thread, must leave the msg_id re-presentable, and
                    // must not be acked. The best-effort relay/carry for
                    // absent members is skipped; the next re-presentation
                    // re-runs the whole branch.
                    Log.w(TAG, "Deferring group envelope from $sourceLabel: durable delivery failed (${e.message})")
                    return finishAdmission(CoreInboundDisposition.FAILED, terminal = false)
                }
                // specs/group-relay-durability.md §4.3 no-reinjection rule:
                // a relay-fetched group message addressed to OUR OWN hint is
                // a per-member fan-out copy -- the relay fan-out already
                // reaches every member durably, so re-flooding/carrying it
                // would give the same content a second flood identity under
                // the fan-out msg_id. Legacy group-hint relay rows and every
                // BLE/LAN-sourced group frame keep the flood+carry behavior.
                val ownFanoutCopy = sourceAddress == null &&
                    coreIsOwnFanoutHint(envelope.recipientHint, identity.userId, System.currentTimeMillis())
                if (!ownFanoutCopy) {
                    relayForeignEnvelope(sourceAddress, envelope)
                    if (sourceAddress == null) {
                        carryRelayEnvelope(envelope)
                    } else {
                        carryForeignEnvelope(envelope, forceFamily = true)
                    }
                }
                // DTN D4: [deliverOpenedGroupEnvelope] durably stores our own
                // copy and throws (rather than returning) on a store
                // failure, so reaching this line means we already have it --
                // record regardless of whether the best-effort mule copy for
                // absent members above was stored.
                return finishAdmission(CoreInboundDisposition.CONSUMED, terminal = true)
            }
            // Not for us (or unopenable) -> foreign traffic. Two jobs, both
            // best-effort (DESIGN.md §5.3): flood it to whoever's connected
            // right now, and carry it so we can hand it to its recipient the
            // next time we meet them, even if that's hours from now.
            relayForeignEnvelope(sourceAddress, envelope)
            val carried = if (sourceAddress == null) {
                carryRelayEnvelope(envelope)
            } else {
                carryForeignEnvelope(envelope)
            }
            // DTN D4: only record once the durable carry actually succeeded.
            // [carryForeignEnvelope]/[carryRelayEnvelope] catch their own
            // store exceptions and report failure via their Boolean return
            // instead of throwing, so a disk-full failure here leaves this
            // msg_id unrecorded: the next copy of this envelope on any link
            // re-gates as Dispatch and gets another chance to carry it,
            // instead of being silently dropped as Seen for the rest of the
            // process lifetime.
            return finishAdmission(CoreInboundDisposition.CARRIED, terminal = carried)
        }
        val arrival = messageArrival(sourceAddress, envelope.hopTtl, opened.senderUserId)
        try {
            deliverOpenedEnvelope(sourceLabel, sourceAddress != null, opened, identity, arrival, envelope.msgId)
        } catch (e: CoreException) {
            // T4-06: [deliverOpenedEnvelope] does not swallow store exceptions
            // (see [handleIncomingChatMessage] etc.), so a throw here means a
            // message that was OURS to open failed to persist (disk full,
            // corrupt store). Translate it instead of letting it unwind: the
            // receive thread / relay batch loop must not be torn down, the
            // msg_id stays unrecorded so the next copy re-dispatches, and
            // FAILED is never acked so the relay copy survives for that retry.
            Log.w(TAG, "Deferring envelope from $sourceLabel: durable delivery failed (${e.message})")
            return finishAdmission(CoreInboundDisposition.FAILED, terminal = false)
        }
        // DTN D4: reaching here means the message was durably stored -- safe,
        // and required, to record.
        return finishAdmission(CoreInboundDisposition.CONSUMED, terminal = true)
    }

    private fun messageArrival(
        sourceAddress: String?,
        receivedHopTtl: UByte,
        senderUserId: ByteArray,
    ): MessageArrival {
        val linkPeerMatchesSender = sourceAddress
            ?.let(MeshRouter::userIdFor)
            ?.contentEquals(senderUserId) == true
        val linkTransport = sourceAddress?.let(MeshRouter::transportFor)
        return MessageArrival(
            transport = arrivalTransport(sourceAddress == null, linkPeerMatchesSender, linkTransport),
            hopsTaken = arrivalHopsTaken(receivedHopTtl, DEFAULT_HOP_TTL),
            receivedAt = System.currentTimeMillis(),
        )
    }

    /**
     * Opens [sealed] with any imported group whose recent-day `recipient_hint`
     * matches [recipientHint]. Returns the matching [Group] and opened
     * payload, or null. [openGroupMessage] does not check membership of the
     * signer; callers must enforce that before trusting the body.
     */
    private fun tryOpenGroupMessage(
        recipientHint: ByteArray,
        sealed: ByteArray,
    ): Pair<Group, OpenedMessage>? {
        val now = System.currentTimeMillis()
        for (group in store.groupsMatchingHint(recipientHint, now)) {
            try {
                return group to openGroupMessage(group, sealed)
            } catch (_: CoreException) {
                // Wrong key / corrupt — try the next matching group (rare).
            }
        }
        return null
    }

    /**
     * Adds a foreign envelope to the persistent carry queue (DESIGN.md §5.3
     * store-and-forward). Classifies it as "family" -- addressed to someone we
     * know -- when its `recipient_hint` matches a contact ([MessageStore.hintMatchesKnownTarget]);
     * family envelopes win eviction fights, while foreign ones share a bounded
     * [FOREIGN_CARRY_BUDGET_BYTES] budget and the core bounds the whole queue.
     * Idempotent on `msg_id`, so re-seeing an envelope we already carry is a
     * no-op. Reached only after [processInboundEnvelope]'s dedupe + expiry gates, so
     * we never carry a stale duplicate or an already-expired envelope.
     *
     * The stored `hop_ttl` is [carriedHopTtl] of the received value, not the
     * value verbatim: this device's carry of the envelope is itself a hop, so
     * it must be counted like the flood path counts its own re-relays (see
     * [relayForeignEnvelope]) -- otherwise [arrivalHopsTaken] under-counts a
     * pure mule delivery by one. See [carriedHopTtl]'s KDoc for the full
     * rationale and the zero-TTL saturation guarantee.
     *
     * Returns `true` if the store operation completed (whether it newly
     * queued the envelope or found it already carried) and `false` if the
     * store call itself failed. DTN D4: [processInboundEnvelope] uses this
     * return value to decide whether it's safe to mark the envelope's
     * `msg_id` seen -- see its KDoc.
     */
    private fun carryForeignEnvelope(envelope: Frame.Envelope, forceFamily: Boolean = false): Boolean {
        val now = System.currentTimeMillis()
        return try {
            val isFamily = forceFamily || store.hintMatchesKnownTarget(envelope.recipientHint, now)
            val stored = store.enqueueCarriedEnvelope(
                CarriedEnvelope(
                    msgId = envelope.msgId,
                    hopTtl = carriedHopTtl(envelope.hopTtl),
                    expiry = envelope.expiry,
                    recipientHint = envelope.recipientHint,
                    sealed = envelope.sealed,
                ),
                isFamily,
                now,
                FOREIGN_CARRY_BUDGET_BYTES,
            )
            if (stored) {
                Log.i(TAG, "Carrying foreign envelope (family=$isFamily) for later delivery")
                if (isFamily) {
                    requestRelaySync("family carry queued")
                }
            }
            true
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to enqueue carried envelope: ${e.message}")
            false
        }
    }

    /**
     * Relay-sourced twin of [carryForeignEnvelope]: adds an envelope we
     * fetched FROM the relay (relay proxy-polling, [MessageStore.relayProxyHints]) to the
     * persistent carry queue for BLE delivery to its real recipient. Unlike
     * [carryForeignEnvelope], this deliberately does NOT call
     * [requestRelaySync] -- the envelope is already sitting on the relay (that
     * is where we just fetched it from), so re-uploading it would only churn
     * traffic and risk resurrecting a copy the real recipient already acked.
     * [MessageStore.enqueueRelayCarriedEnvelope] enforces this on the core
     * side too (`from_relay = 1` is excluded from the upload query), so this
     * is belt-and-suspenders, but skipping the call here avoids scheduling a
     * pointless relay-sync pass. Idempotent on `msg_id` like its sibling.
     *
     * Also mirrors [carryForeignEnvelope] in storing [carriedHopTtl] of the
     * received `hop_ttl` rather than the raw value -- this device is muling
     * the envelope the same as the BLE-sourced case, so the same hop must be
     * counted.
     *
     * Returns `true`/`false` on store success/failure -- see
     * [carryForeignEnvelope]'s KDoc for why [processInboundEnvelope] needs
     * this (DTN D4).
     */
    private fun carryRelayEnvelope(envelope: Frame.Envelope): Boolean {
        val now = System.currentTimeMillis()
        return try {
            val stored = store.enqueueRelayCarriedEnvelope(
                CarriedEnvelope(
                    msgId = envelope.msgId,
                    hopTtl = carriedHopTtl(envelope.hopTtl),
                    expiry = envelope.expiry,
                    recipientHint = envelope.recipientHint,
                    sealed = envelope.sealed,
                ),
                now,
            )
            if (stored) {
                Log.i(TAG, "Carrying relay-sourced envelope (proxy) for later BLE delivery")
            }
            true
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to enqueue relay-carried envelope: ${e.message}")
            false
        }
    }

    /**
     * Hands over every carried envelope destined for the peer that just
     * HELLO'd on [address] (DESIGN.md §5.3): we compute the peer's recent-day
     * `recipient_hint`s ([recentHintsFor]) and pull matching envelopes from
     * the store, and send each on this link. Expired entries are pruned
     * first. If the peer already saw an envelope via an earlier flood, their
     * own seen-ID set drops the duplicate harmlessly; if they didn't (the
     * whole point -- they were out of range when it flooded), this is how it
     * reaches them.
     *
     * `env.hopTtl` here is forwarded verbatim -- it's already [carriedHopTtl]
     * of what this device originally received, decremented once at
     * [carryForeignEnvelope]/[carryRelayEnvelope] enqueue time, not the raw
     * value the frame arrived with. No further decrement happens here.
     *
     * DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): this function only ever
     * *attempts* delivery -- it no longer calls [MessageStore.removeCarriedEnvelope]
     * on a successful [MeshRouter.sendToAddress]. That return only means a
     * transport function accepted the write (e.g. [BleCentral]'s `sendFrame`
     * just enqueues fragments into a per-address write queue), not that the
     * bytes made it to the peer; a disconnect mid-transfer used to silently
     * drop the whole write queue after we'd already deleted our only copy.
     * The carried row is now removed later, once the peer's own next digest
     * exchange proves they actually have it -- see
     * [MessageStore.coreConfirmCarriedDeliveries], called from
     * [sprayDigestPlanTo].
     *
     * Invariant, stated verbatim (DTN_TODOS.md §3.2): worst case of a
     * dropped mid-transfer link is a harmless duplicate resend (the peer's
     * seen-set/store dedupes it), never a lost envelope; an unconfirmed
     * carry still dies at its normal expiry via [MessageStore.pruneExpiredCarried].
     */
    fun drainCarriedEnvelopesTo(address: String, peerUserId: ByteArray) {
        val now = System.currentTimeMillis()
        try {
            store.pruneExpiredCarried(now)
            // Peer userId hints plus every group that peer is a member of
            // (DESIGN.md §6.5: members mule for the whole group).
            val deliveryHints = store.deliveryHintsForPeer(peerUserId, now)
            val toDeliver = store.carriedEnvelopesForHints(deliveryHints, now)
            if (toDeliver.isEmpty()) return
            var delivered = 0
            for (env in toDeliver) {
                val frame = encodeEnvelopeFrame(env.msgId, env.hopTtl, env.expiry, env.recipientHint, env.sealed)
                if (MeshRouter.sendToAddress(address, frame)) {
                    delivered++
                }
            }
            Log.i(TAG, "Attempted delivery of $delivered carried envelope(s) to $address (removal awaits their digest confirmation)")
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to drain carried envelopes to $address: ${e.message}")
        }
    }

    /**
     * Floods a foreign (not-for-us) envelope onward per DESIGN.md §5.3, if it
     * still has hop budget. `hop_ttl` is the remaining number of hops; we
     * decrement it and forward only while at least one hop would remain
     * (`hop_ttl > 1`), so a frame arriving with `hop_ttl == 1` is the last
     * carrier's copy and stops here. The `msg_id`, `expiry`, `recipient_hint`,
     * and sealed bytes are all preserved verbatim -- only `hop_ttl` changes --
     * so every carrier along the way computes the same dedupe key. The
     * arriving link is excluded from the flood to avoid the trivial echo;
     * the mesh's other seen-ID sets stop longer loops once the recipients
     * record this `msg_id` themselves.
     *
     * DTN D4 / FA5 loop-hazard note: since [processInboundEnvelope] moved to
     * check-then-record, [GossipState.seenIds] is *not yet* updated for this
     * `msg_id` at the moment this call happens (it's recorded, via
     * [InboundEnvelopeAdmission.finish], after this function returns, once
     * the whole terminal branch succeeds -- see [processInboundEnvelope]'s
     * KDoc). This is still safe against self-re-ingestion, but *not* for the
     * reason an earlier version of this note claimed:
     * [processInboundEnvelope] does **not** run synchronously per received
     * frame -- it is called concurrently from up to four receive-path
     * threads (central-GATT binder, peripheral-GATT binder, LanTransport's
     * `connectionExecutor`, and the relay-sync thread), and two copies of one
     * `msg_id` arriving on different transports at once is routine for a
     * nearby contact. What actually rules out same-node re-entrancy for
     * *this* `msg_id` before the terminal record lands is
     * [InboundEnvelopeAdmission]'s atomic in-flight claim: a concurrent
     * second copy of this exact `msg_id`, on any thread, is rejected at the
     * top of [processInboundEnvelope] before it ever reaches this function.
     * Combined with the arriving link being excluded from the fanout above
     * (so this node can't hand the relayed frame straight back to itself),
     * a frame this node relays could only loop back from a third node's
     * rebroadcast, which takes at least one more hop and one more link
     * round-trip -- by then this node's record has long since happened.
     */
    private fun relayForeignEnvelope(address: String?, envelope: Frame.Envelope) {
        val remainingHops = envelope.hopTtl.toInt()
        if (remainingHops <= 1) {
            // Hop budget exhausted; this node is the final carrier for it.
            return
        }
        val relayed = encodeEnvelopeFrame(
            envelope.msgId,
            (remainingHops - 1).toUByte(),
            envelope.expiry,
            envelope.recipientHint,
            envelope.sealed,
        )
        val fanout = if (address == null) {
            MeshRouter.relayToAll(relayed)
        } else {
            MeshRouter.relayToAllExcept(address, relayed)
        }
        if (fanout > 0) {
            Log.i(
                TAG,
                "Relayed foreign envelope from ${address ?: "relay"} to $fanout link(s), " +
                    "hop_ttl ${remainingHops}->${remainingHops - 1}",
            )
        }
    }

    /**
     * Delivers an envelope we successfully opened (DESIGN.md §6.3 open/verify,
     * §7.1 body layout). See this class's KDoc for why
     * `body.chatId == opened.senderUserId` is the correct sanity check here.
     * Reached only for envelopes addressed to us; foreign traffic never gets
     * here (see [processInboundEnvelope]).
     */
    private fun deliverOpenedEnvelope(
        address: String,
        directBle: Boolean,
        opened: OpenedMessage,
        identity: Identity,
        arrival: MessageArrival,
        msgId: ByteArray,
    ) {
        val extendedBody = try {
            decodeExtendedMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping envelope from $address: failed to decode body (${e.message})")
            return
        }
        val body = MessageBody(
            kind = extendedBody.kind,
            chatId = extendedBody.chatId,
            lamport = extendedBody.lamport,
            timestamp = extendedBody.timestamp,
            content = extendedBody.content,
        )
        if (!body.chatId.contentEquals(opened.senderUserId)) {
            Log.w(TAG, "Dropping envelope from $address: chatId does not match the verified sender")
            return
        }
        val senderIsContact = store.getContact(opened.senderUserId) != null
        if (
            !corePairwiseSenderAuthorized(
                body.kind,
                senderIsContact,
                opened.senderUserId.contentEquals(identity.userId),
            )
        ) {
            Log.w(TAG, "Dropping envelope from $address: sender is not authorized for kind=${body.kind}")
            return
        }

        when (body.kind) {
            KIND_TEXT -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_TEXT,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_ATTACHMENT_MANIFEST -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_ATTACHMENT_MANIFEST,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_REACTION -> handleIncomingChatMessage(
                address,
                opened.senderUserId,
                body,
                identity,
                KIND_REACTION,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_RECEIPT -> handleIncomingReceipt(
                address,
                opened.senderUserId,
                body,
                identity,
                arrival,
            )
            KIND_FRIEND_REQUEST -> handleIncomingFriendRequest(address, directBle, opened.senderUserId, body, identity)
            KIND_GROUP_INVITE -> handleIncomingGroupInvite(address, opened.senderUserId, body, identity)
            KIND_PROFILE_SYNC -> handleIncomingProfileSync(address, opened.senderUserId, body, identity)
            KIND_FRIEND_DIRECTORY -> handleIncomingFriendDirectory(address, opened.senderUserId, body, identity)
            KIND_INTRODUCED_FRIEND_REQUEST -> handleIncomingIntroducedFriendRequest(
                address,
                directBle,
                opened.senderUserId,
                body,
                identity,
            )
            KIND_LAN_ENDPOINT_HINT -> handleIncomingLanEndpointHint(
                address,
                opened.senderUserId,
                body,
                identity,
            )
            else -> Log.i(TAG, "Dropping envelope from $address: unhandled kind=${body.kind}")
        }
    }

    private fun handleIncomingLanEndpointHint(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val contact = store.getContact(senderUserId) ?: return
        val content = try {
            decodeLanEndpointContent(body.content)
        } catch (error: CoreException) {
            Log.w(TAG, "Dropping sealed LAN endpoint hint: ${error.message}")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_LAN_ENDPOINT_HINT,
                payload = body.content,
            ),
        )
        if (!inserted) return

        val hintedNetworkId = content.networkId.toString(Charsets.UTF_8)
        val endpoint = LanManualEndpoint(content.host, content.port.toInt())
        lan.saveLanEndpoint(hintedNetworkId, senderUserId, endpoint)
        LanCapabilityStore.markSupported(context, senderUserId)
        val now = System.currentTimeMillis()
        if (
            content.expiresAtMs > now &&
            hintedNetworkId == lan.currentLanNetworkId()
        ) {
            lan.connectToLanHint(
                Frame.LanEndpoint(
                    instanceToken = content.instanceToken,
                    host = content.host,
                    port = content.port,
                ),
                senderUserId,
            )
        }
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
    }

    /**
     * Delivers a group-sealed envelope we opened with an imported group key
     * (DESIGN.md §6.5). Wire [MessageBody.chatId] is the group id; the
     * verified signer must be a current member (core does not check this).
     * Group receipts are deferred — we only store + notify.
     */
    private fun deliverOpenedGroupEnvelope(
        address: String,
        group: Group,
        opened: OpenedMessage,
        identity: Identity,
        arrival: MessageArrival,
        msgId: ByteArray,
    ) {
        if (!group.memberUserIds.any { it.contentEquals(opened.senderUserId) }) {
            Log.w(
                TAG,
                "Dropping group envelope from $address: signer ${UserIdHex.encode(opened.senderUserId)} " +
                    "is not a member of group ${group.name}",
            )
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) {
            Log.w(TAG, "Dropping group envelope from $address: we are not a member of ${group.name}")
            return
        }

        val extendedBody = try {
            decodeExtendedMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping group envelope from $address: failed to decode body (${e.message})")
            return
        }
        val body = MessageBody(
            kind = extendedBody.kind,
            chatId = extendedBody.chatId,
            lamport = extendedBody.lamport,
            timestamp = extendedBody.timestamp,
            content = extendedBody.content,
        )
        if (!body.chatId.contentEquals(group.id)) {
            Log.w(TAG, "Dropping group envelope from $address: body.chatId does not match group id")
            return
        }
        when (body.kind) {
            KIND_TEXT, KIND_ATTACHMENT_MANIFEST, KIND_REACTION -> handleIncomingGroupChatMessage(
                address,
                group,
                opened.senderUserId,
                body,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            KIND_GROUP_METADATA_UPDATE -> handleIncomingGroupMetadataUpdate(
                address,
                group,
                opened.senderUserId,
                body,
                arrival,
                msgId,
                extendedBody.replyToMsgId,
            )
            else -> Log.i(TAG, "Dropping group envelope from $address: unhandled kind=${body.kind}")
        }
    }

    private fun handleIncomingGroupMetadataUpdate(
        address: String,
        group: Group,
        senderUserId: ByteArray,
        body: MessageBody,
        arrival: MessageArrival,
        msgId: ByteArray,
        replyToMsgId: ByteArray?,
    ) {
        val updated = try {
            val update = decodeGroupMetadataUpdate(body.content)
            applyGroupMetadataUpdate(group, update, senderUserId)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping invalid group metadata from $address: ${e.message}")
            return
        }
        val inserted = store.insertIncomingMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = body.kind,
                payload = body.content,
            ),
            msgId,
            replyToMsgId,
        )
        if (!inserted) return
        store.recordMessageArrival(group.id, senderUserId, body.lamport, arrival)
        if (updated != null) {
            store.upsertGroup(updated)
            Log.i(TAG, "Applied group metadata revision ${updated.metadataRevision} for ${updated.name}")
            ChatEvents.notifyChatChanged(group.id)
        }
    }

    private fun handleIncomingGroupChatMessage(
        address: String,
        group: Group,
        senderUserId: ByteArray,
        body: MessageBody,
        arrival: MessageArrival,
        msgId: ByteArray,
        replyToMsgId: ByteArray?,
    ) {
        val inserted = store.insertIncomingMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = body.kind,
                payload = body.content,
            ),
            msgId,
            replyToMsgId,
        )
        if (!inserted) {
            Log.i(
                TAG,
                "Ignoring duplicate group kind=${body.kind} from $address " +
                    "sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
            )
            return
        }
        store.recordMessageArrival(group.id, senderUserId, body.lamport, arrival)
        Log.i(
            TAG,
            "Stored group kind=${body.kind} in ${group.name} from $address " +
                "sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
        ChatEvents.notifyChatChanged(group.id)

        // Local read watermark only (group wire receipts are deferred). Uses
        // highestLamport (plain MAX), not highestContiguousLamport: the
        // latter stalls at 0 once the sender's stream legitimately starts
        // above lamport 1 (post chat-history-wipe ratchet), which would
        // leave this watermark -- and the unread badge -- stuck forever.
        val throughLamport = store.highestLamport(group.id, senderUserId)
        store.recordOutgoingReceipt(group.id, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        val isVisible = ChatVisibility.isVisible(group.id)
        if (isVisible) {
            store.recordOutgoingReceipt(group.id, senderUserId, RECEIPT_TYPE_READ, throughLamport)
        } else if (isVisibleChatKind(body.kind)) {
            val senderName = store.getContact(senderUserId)?.let(::coreContactDisplayName)
                ?: UserIdHex.encode(senderUserId).take(8)
            val preview = if (body.kind == KIND_ATTACHMENT_MANIFEST) {
                try {
                    AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                } catch (_: Exception) {
                    "Attachment"
                }
            } else {
                body.content.toString(Charsets.UTF_8)
            }
            MessageNotifier.notifyIncomingGroupMessage(context, group, senderName, preview)
        }
    }

    /**
     * Imports a pairwise-sealed `kind=4` group invite (DESIGN.md §6.5). Wire
     * `chatId` is the invite sender's userId (1:1 pairwise convention); the
     * group id/key/members live in the invite content. Local history is stored
     * under `chat_id = group.id`.
     */
    private fun handleIncomingGroupInvite(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val group = try {
            decodeGroupInviteContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping group invite from $address: failed to decode (${e.message})")
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) {
            Log.w(TAG, "Dropping group invite from $address: we are not listed as a member")
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(senderUserId) }) {
            Log.w(TAG, "Dropping group invite from $address: sender is not listed as a member")
            return
        }

        store.upsertGroup(group)
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = group.id,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_GROUP_INVITE,
                payload = body.content,
            ),
        )
        if (!inserted) {
            Log.i(TAG, "Ignoring duplicate group invite for ${group.name} from $address")
            return
        }
        ChatEvents.notifyChatChanged(group.id)
        Log.i(TAG, "Imported group ${group.name} from invite on $address")

        val contact = store.getContact(senderUserId)
        if (contact != null) {
            // 1:1 delivered/read receipts still apply to the pairwise invite
            // envelope's sender stream — but the invite row lives under the
            // group chat, so we only ack if we also store something under the
            // 1:1 chat. Skip wire receipts for invites; the group is what matters.
        }
        if (!ChatVisibility.isVisible(group.id)) {
            // FA8: a typed entry point, not a literal string sniffed by
            // MessageNotifier's prefix check -- see notifyGroupInvite's KDoc.
            MessageNotifier.notifyGroupInvite(context, group)
        }
    }

    /**
     * Stores a signed `kind=3` friend request in the hidden lamport stream and
     * imports/updates the sender as a contact from the authenticated payload.
     * The payload is a FriendCard JSON string, but unlike a QR scan we can
     * verify it matches the envelope sender's signing key before trusting it.
     */
    private fun handleIncomingFriendRequest(
        address: String,
        directBle: Boolean,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val pendingSuggestion = store.listFriendSuggestions(System.currentTimeMillis()).firstOrNull {
            it.state == 1.toUByte() && it.candidate.userId.contentEquals(senderUserId)
        }
        val card = try {
            parseFriendCard(body.content.toString(Charsets.UTF_8))
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping friend request from $address: failed to parse FriendCard (${e.message})")
            return
        }
        if (!friendCardUserId(card).contentEquals(senderUserId)) {
            Log.w(TAG, "Dropping friend request from $address: payload identity doesn't match verified sender")
            return
        }

        val wasKnown = store.getContact(senderUserId) != null
        val contact = RelayImport.reconcileOnImport(
            context,
            store,
            Contact(
                userId = senderUserId,
                name = card.name,
                signPk = card.signPk,
                agreePk = card.agreePk,
                relayUrl = card.relayUrl,
                relayToken = card.relayToken,
            ),
        )
        store.upsertContactProvenance(
            ContactProvenance(
                userId = senderUserId,
                source = if (pendingSuggestion == null) 0u else 1u,
                introducerUserId = pendingSuggestion?.introducerUserId,
                introducedAtMs = System.currentTimeMillis(),
            ),
        )
        if (pendingSuggestion != null) store.removeFriendSuggestion(senderUserId)
        ProfileSyncSender.queueToContact(
            context,
            store,
            identity,
            contact,
            ProfileStore.loadOwnAvatarEpoch(context),
        )
        lan.sendLanEndpointHintTo(address)
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_FRIEND_REQUEST,
                payload = body.content,
            ),
        )
        if (!inserted) return
        ChatEvents.notifyChatChanged(senderUserId)

        // highestLamport (plain MAX), not highestContiguousLamport: this is
        // a watermark over the peer's stream, and after the lamport ratchet
        // that stream can legitimately start above 1, where the contiguous
        // count would stall at 0 forever.
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        val isVisible = ChatVisibility.isVisible(senderUserId)
        if (isVisible) {
            store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_READ, throughLamport)
            relayQueueChanged = queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = senderUserId,
                throughLamport = throughLamport,
            ) || relayQueueChanged
        }
        if (relayQueueChanged) {
            RelaySyncEvents.requestSync()
        }

        sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, senderUserId, throughLamport)
        if (isVisible) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        }
        if (!wasKnown) {
            FriendImportEvents.notifyImported(contact, directBle)
            MessageNotifier.notifyFriendAdded(context, contact)
        }
        Log.i(TAG, "Imported contact ${contact.name} from friend request on $address")
    }

    private fun handleIncomingProfileSync(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val existing = store.getContact(senderUserId)
        if (existing == null) {
            Log.i(TAG, "Dropping profile sync from $address: sender is not a contact")
            return
        }
        val content = try {
            decodeProfileSyncContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping profile sync from $address: failed to decode (${e.message})")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_PROFILE_SYNC,
                payload = body.content,
            ),
        )
        if (!inserted) return

        val policyChanged = store.upsertContactDiscoveryPolicy(
            ContactDiscoveryPolicy(
                userId = senderUserId,
                protocolVersion = content.friendsOfFriendsVersion,
                enabled = content.friendsOfFriendsEnabled,
                revision = content.friendsOfFriendsRevision,
            ),
        )

        val applied = store.setContactAvatar(
            senderUserId,
            content.avatar.takeIf { it.isNotEmpty() },
            content.avatarEpoch,
        )
        if (applied && content.name != existing.name) {
            store.upsertContact(existing.copy(name = content.name))
        }
        ChatEvents.notifyChatChanged(senderUserId)

        val contact = store.getContact(senderUserId) ?: existing
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        val isVisible = ChatVisibility.isVisible(senderUserId)
        if (isVisible) {
            store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_READ, throughLamport)
            relayQueueChanged = queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = senderUserId,
                throughLamport = throughLamport,
            ) || relayQueueChanged
        }
        if (relayQueueChanged) {
            RelaySyncEvents.requestSync()
        }

        sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, senderUserId, throughLamport)
        if (isVisible) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        }
        if (policyChanged) {
            FriendDirectorySender.queueToAllContacts(context, store, identity)
        }
    }

    private fun handleIncomingFriendDirectory(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val contact = store.getContact(senderUserId) ?: run {
            Log.i(TAG, "Dropping friend directory from $address: sender is not a contact")
            return
        }
        val content = try {
            decodeFriendDirectoryContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping friend directory from $address: ${e.message}")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_FRIEND_DIRECTORY,
                payload = body.content,
            ),
        )
        if (!inserted) return
        if (FriendsOfFriendsStore.isEnabled(context)) {
            try {
                if (store.applyFriendDirectory(senderUserId, identity.userId, content, System.currentTimeMillis())) {
                    ChatEvents.notifyChatChanged(senderUserId)
                }
            } catch (e: CoreException) {
                Log.w(TAG, "Rejecting friend directory from $address: ${e.message}")
                return
            }
        }
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
    }

    private fun handleIncomingIntroducedFriendRequest(
        address: String,
        directBle: Boolean,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        if (!FriendsOfFriendsStore.isEnabled(context)) {
            Log.i(TAG, "Ignoring introduced friend request while friends-of-friends is disabled")
            return
        }
        val request = try {
            decodeIntroducedFriendRequest(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping introduced friend request from $address: ${e.message}")
            return
        }
        val card = try {
            parseFriendCard(request.friendCardJson)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping introduced friend request with invalid card: ${e.message}")
            return
        }
        if (!friendCardUserId(card).contentEquals(senderUserId)) {
            Log.w(TAG, "Dropping introduced friend request: card does not match authenticated sender")
            return
        }
        val introducer = store.getContact(request.ticket.introducerUserId) ?: run {
            Log.w(TAG, "Dropping introduced friend request: introducer is no longer a contact")
            return
        }
        val valid = try {
            verifyIntroductionTicket(
                request.ticket,
                introducer.signPk,
                identity.userId,
                senderUserId,
                FriendsOfFriendsStore.revision(context),
                System.currentTimeMillis(),
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping introduced friend request: ${e.message}")
            return
        }
        if (!valid) {
            Log.w(TAG, "Dropping introduced friend request: ticket validation failed")
            return
        }

        val wasKnown = store.getContact(senderUserId) != null
        val contact = RelayImport.reconcileOnImport(
            context,
            store,
            Contact(
                userId = senderUserId,
                name = card.name,
                signPk = card.signPk,
                agreePk = card.agreePk,
                relayUrl = card.relayUrl,
                relayToken = card.relayToken,
            ),
        )
        store.upsertContactProvenance(
            ContactProvenance(
                userId = senderUserId,
                source = 1u,
                introducerUserId = introducer.userId,
                introducedAtMs = System.currentTimeMillis(),
            ),
        )
        store.removeFriendSuggestion(senderUserId)
        store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_INTRODUCED_FRIEND_REQUEST,
                payload = body.content,
            ),
        )
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
        FriendRequestSender.queueForScannedContact(context, store, identity, contact)
        ProfileSyncSender.queueToContact(
            context,
            store,
            identity,
            contact,
            ProfileStore.loadOwnAvatarEpoch(context),
        )
        lan.sendLanEndpointHintTo(address)
        if (!wasKnown) FriendDirectorySender.queueToAllContacts(context, store, identity)
        ChatEvents.notifyChatChanged(senderUserId)
        if (!wasKnown) {
            FriendImportEvents.notifyImported(contact, directBle)
            MessageNotifier.notifyFriendAdded(context, contact)
        }
    }

    private fun acknowledgeHiddenMessage(
        address: String,
        senderUserId: ByteArray,
        identity: Identity,
        contact: Contact,
    ) {
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        val queued = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        if (queued) RelaySyncEvents.requestSync()
        sendReceiptOnAddress(
            identity,
            contact,
            address,
            RECEIPT_TYPE_DELIVERED,
            senderUserId,
            throughLamport,
        )
    }

    /**
     * Stores an incoming text message and, only if it was newly inserted,
     * sends a delivered receipt back on the same link (DESIGN.md §7.2), plus
     * -- if the chat is currently on screen ([ChatVisibility.isVisible]) -- a
     * read receipt too. Otherwise, posts a notification
     * ([MessageNotifier.notifyIncomingMessage]) instead, since the chat isn't
     * visible for the user to see the message land. Those two are mutually
     * exclusive by construction (`if (visible) read-receipt else notify`),
     * which matches the product intent: no point notifying about a chat the
     * user is already looking at, and no point sending a read receipt for
     * one they aren't.
     *
     * A duplicate insert (e.g. re-sent by the peer's digest sync,
     * DESIGN.md §7.3) is a silent no-op here -- it was already acknowledged
     * (and, if applicable, notified) the first time, and redoing either
     * wouldn't change anything, so this path can never send two receipts or
     * two notifications for one message.
     *
     * This never triggers another receipt (see [handleIncomingReceipt]):
     * receipts are kind=2, this branch only ever runs for chat-stream kinds
     * (text / attachment-manifest), and [handleIncomingReceipt] never calls
     * [sendReceiptOnAddress] or [sendReceiptToContact] or otherwise sends
     * anything back. Combined with authored resend only ever replaying kinds
     * that *we* originated (text, attachment, friend-request — never a
     * receipt), there's no cycle where a receipt causes a receipt.
     */
    private fun handleIncomingChatMessage(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
        kind: UByte,
        arrival: MessageArrival,
        msgId: ByteArray,
        replyToMsgId: ByteArray?,
    ) {
        val inserted = store.insertIncomingMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = kind,
                payload = body.content,
            ),
            msgId,
            replyToMsgId,
        )
        if (!inserted) {
            Log.i(
                TAG,
                "Ignoring duplicate kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
            )
            return
        }
        store.recordMessageArrival(senderUserId, senderUserId, body.lamport, arrival)
        Log.i(
            TAG,
            "Stored kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(senderUserId), System.currentTimeMillis())
        ChatEvents.notifyChatChanged(senderUserId)

        // highestLamport (plain MAX), not highestContiguousLamport: same
        // peer-stream-watermark reasoning as above -- a contiguous-from-1
        // count stalls at 0 once the sender's stream starts above 1 (post
        // ratchet), stranding the delivered/read receipt and unread badge.
        val throughLamport = store.highestLamport(senderUserId, senderUserId)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = false
        val isVisible = ChatVisibility.isVisible(senderUserId)
        if (isVisible) {
            store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_READ, throughLamport)
        }

        val contact = store.getContact(senderUserId)
        if (contact == null) {
            // We stored the message (friending can happen independently of
            // messaging order), but with no contact we have no agreePk to
            // seal a receipt to, and nothing sensible to show in a
            // notification (no display name, no key to trust it came from
            // who it claims), so skip both.
            Log.i(TAG, "Stored a message from unrecognized userId=${UserIdHex.encode(senderUserId)}; no receipt/notification")
            return
        }

        relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        if (isVisible) {
            relayQueueChanged = queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = senderUserId,
                throughLamport = throughLamport,
            ) || relayQueueChanged
        }
        if (relayQueueChanged) {
            RelaySyncEvents.requestSync()
        }

        sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_DELIVERED, senderUserId, throughLamport)

        if (isVisible) {
            // The user is already looking at this chat, so it was read the
            // instant it landed -- send the read receipt now rather than
            // waiting for ChatViewEvents, which only fires when a chat
            // *becomes* visible, not for messages arriving while it already is.
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        } else if (isVisibleChatKind(kind)) {
            val preview = when (kind) {
                KIND_ATTACHMENT_MANIFEST ->
                    AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                else -> body.content.toString(Charsets.UTF_8)
            }
            MessageNotifier.notifyIncomingMessage(context, contact, preview)
        }
    }

    /**
     * Persists an incoming receipt as a delivered/read watermark on our own
     * outgoing messages (DESIGN.md §7.2) and pings [ChatEvents] so any open
     * chat screen redraws its ✓/✓✓ ticks.
     *
     * Two sanity checks before trusting it, both log-and-drop on failure:
     * - `receipt.senderUserId` must be OUR OWN userId. A receipt only ever
     *   acknowledges messages *we* authored in a 1:1 chat -- a peer has no
     *   business acking someone else's messages to us, so anything else here
     *   is either a bug or a malicious/confused peer.
     * - The outer envelope's verified sender ([envelopeSenderUserId], from
     *   [processInboundEnvelope]'s `openMessage`) must be a known contact, since
     *   that's the local `chatId` this receipt gets recorded under (see
     *   below) and we only track receipts for chats we actually have.
     *
     * `store.recordReceipt`'s `senderUserId` param is OUR OWN userId here --
     * not [envelopeSenderUserId] -- because it names whose *messages* the
     * receipt is about (ours), while `chatId` is [envelopeSenderUserId]
     * because locally a 1:1 chat is keyed by the other party (see class
     * KDoc). This never sends anything back, so it cannot loop into another
     * receipt (see [handleIncomingChatMessage]'s KDoc for the full argument).
     */
    private fun handleIncomingReceipt(
        address: String,
        envelopeSenderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
        arrival: MessageArrival,
    ) {
        val receipt = try {
            decodeReceiptContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping receipt from $address: failed to decode (${e.message})")
            return
        }
        if (!receipt.senderUserId.contentEquals(identity.userId)) {
            Log.w(
                TAG,
                "Dropping receipt from $address: acks senderUserId=${UserIdHex.encode(receipt.senderUserId)}, " +
                    "not us -- peers can only ack messages we authored",
            )
            return
        }
        val contact = store.getContact(envelopeSenderUserId)
        if (contact == null) {
            Log.w(TAG, "Dropping receipt from $address: envelope sender ${UserIdHex.encode(envelopeSenderUserId)} is not a known contact")
            return
        }

        Log.i(
            TAG,
            "Receipt from $address: ackedSender=${UserIdHex.encode(receipt.senderUserId)} " +
                "throughLamport=${receipt.lamport} type=${receipt.receiptType}",
        )
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(envelopeSenderUserId), System.currentTimeMillis())
        // The receipt returned on the exact link that delivered the message;
        // record that route against the watermark (T6) so every acknowledged
        // message's Info pane can prove LAN/BLE/relay delivery -- not just the
        // one at the exact watermark lamport.
        store.recordReceipt(
            chatId = envelopeSenderUserId, // local convention: chat keyed by the other party -- see class KDoc
            senderUserId = identity.userId, // whose messages this receipt is about: ours
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
            viaTransport = arrival.transport,
        )
        // V2 field metric: stamp delivery latency + route on the messages this
        // (cumulative) delivery receipt confirms. READ receipts imply delivery
        // too, but the DELIVERED watermark is the one we measure against.
        if (receipt.receiptType == RECEIPT_TYPE_DELIVERED) {
            runCatching {
                store.recordDeliveredMetric(
                    chatId = envelopeSenderUserId,
                    throughLamport = receipt.lamport,
                    deliveredAtMs = arrival.receivedAt,
                    viaTransport = arrival.transport,
                )
            }
        }
        ChatEvents.notifyChatChanged(envelopeSenderUserId)
    }

    /**
     * Persist the latest relay-uploadable sealed receipt envelope for one
     * cumulative outgoing watermark. Same watermark is a no-op so the stored
     * `msg_id` stays stable; higher watermark replaces it with a newly sealed
     * envelope and clears the relay-posted marker in core.
     */
    private fun queueOutgoingReceiptForRelay(
        identity: Identity,
        contact: Contact,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
        timestamp: Long = System.currentTimeMillis(),
    ): Boolean {
        if (throughLamport == 0uL) return false
        val existing = store.outgoingReceiptEnvelope(contact.userId, ackedSenderUserId, receiptType)
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            timestamp,
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        return existing == null || existing.throughLamport < authored.envelope.throughLamport
    }

    /**
     * [RelaySyncEngine.performRelaySyncPass]'s pre-upload receipt backfill,
     * now computed in core ([MessageStore.backfillOutgoingReceiptEnvelopes]):
     * core refreshes every contact's DELIVERED/READ receipt envelopes for the
     * current watermarks and returns their msg_ids, which are recorded into
     * the in-memory seen-set here for the same reason every shell-side
     * receipt authoring records there -- our own receipt envelope coming back
     * off the relay must dedupe, not get re-carried as foreign mail.
     */
    fun backfillRelayOutgoingReceiptEnvelopes(identity: Identity, now: Long) {
        for (msgId in store.backfillOutgoingReceiptEnvelopes(identity, now)) {
            GossipState.seenIds.record(msgId)
        }
    }

    /**
     * [ChatViewEvents] handler: the user just opened [peerUserId]'s chat.
     * Sends a READ receipt covering everything currently stored from that
     * peer (DESIGN.md §7.2), via [sendReceiptToContact] rather than
     * [sendReceiptOnAddress] since there's no specific link this was
     * triggered from -- it goes out over whatever link [MeshRouter] can
     * currently reach the contact on, if any.
     *
     * Best-effort immediately like every receipt: if the peer isn't connected
     * right now, the send simply no-ops (logged at INFO).
     * The difference from the earlier milestone is that the cumulative read
     * watermark is first persisted via `recordOutgoingReceipt`, so the next
     * digest sync re-sends it receipts-first and closes the old retry gap.
     */
    fun handleChatViewed(peerUserId: ByteArray) {
        val identity = identityProvider() ?: return
        val contact = store.getContact(peerUserId) ?: return
        // highestLamport (plain MAX), not highestContiguousLamport: the
        // latter counts contiguously from lamport 1 and returns 0 at the
        // first hole, but the lamport ratchet lets a peer's stream
        // legitimately start above 1 after a chat history wipe (lamports
        // below the new base never existed for anyone). A receiver holding
        // e.g. {3, 4} from that peer would get 0 from the contiguous count
        // forever, so opening the chat would never clear the unread badge
        // or advance the read tick. MAX correctly reflects what we actually
        // hold. The `== 0` guard below still means "nothing received yet,"
        // since MAX is 0 only when the store truly has no message from
        // this peer.
        val throughLamport = store.highestLamport(peerUserId, peerUserId)
        if (throughLamport == 0uL) return // nothing received from this peer yet to ack as read
        store.recordOutgoingReceipt(peerUserId, peerUserId, RECEIPT_TYPE_READ, throughLamport)
        if (
            queueOutgoingReceiptForRelay(
                identity = identity,
                contact = contact,
                receiptType = RECEIPT_TYPE_READ,
                ackedSenderUserId = peerUserId,
                throughLamport = throughLamport,
            )
        ) {
            RelaySyncEvents.requestSync()
        }
        sendReceiptToContact(identity, contact, RECEIPT_TYPE_READ, peerUserId, throughLamport)
    }

    /** Builds a [uniffi.cruisemesh_core.ReceiptContent] and sends it as a sealed envelope on the exact link [address] (a reply to a frame that just arrived on it). */
    private fun sendReceiptOnAddress(
        identity: Identity,
        contact: Contact,
        address: String,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
    ) {
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        MeshRouter.sendToAddress(address, authored.frame)
    }

    /** Builds a [uniffi.cruisemesh_core.ReceiptContent] and sends it to whichever live link currently reaches [contact], if any -- see [handleChatViewed]. */
    private fun sendReceiptToContact(
        identity: Identity,
        contact: Contact,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
    ) {
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        if (!MeshRouter.sendToUserId(contact.userId, authored.frame)) {
            Log.i(TAG, "Receipt to ${UserIdHex.encode(contact.userId)} queued; not currently connected")
        }
    }

    /** Executes Rust's complete digest-time mule plan. */
    fun sprayDigestPlanTo(
        address: String,
        peerUserId: ByteArray,
        peerKnownMsgIds: List<ByteArray>,
        identity: Identity,
    ) {
        val now = System.currentTimeMillis()
        try {
            // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): confirm delivery
            // of anything this digest's advertised `msg_id`s prove the peer
            // already has BEFORE building the spray plan below, so a
            // just-confirmed carried envelope isn't immediately re-sprayed
            // back at the peer who just told us they have it.
            val confirmed = store.coreConfirmCarriedDeliveries(peerUserId, peerKnownMsgIds, now)
            if (confirmed > 0uL) {
                Log.i(TAG, "Confirmed delivery of $confirmed carried envelope(s) to ${UserIdHex.encode(peerUserId)}; dropped our copy")
            }
            val plan = store.coreDigestSprayPlan(
                ownUserId = identity.userId,
                peerUserId = peerUserId,
                peerHints = recentHintsFor(peerUserId, now),
                peerKnownMsgIds = peerKnownMsgIds,
                nowMs = now,
                ownOutboundBudgetBytes = OWN_OUTBOUND_SPRAY_BUDGET_BYTES.toULong(),
                ownReceiptBudgetBytes = OWN_RECEIPT_SPRAY_BUDGET_BYTES.toULong(),
                receiptQueryLimit = RELAY_STORE_BATCH_LIMIT,
            )
            val frames = plan.carriedFrames + plan.ownOutboundFrames + plan.ownReceiptFrames
            val sprayed = frames.count { MeshRouter.sendToAddress(address, it) }
            Log.i(
                TAG,
                "Digest spray to $address sent $sprayed/${frames.size} frame(s) " +
                    "(carried=${plan.carriedFrames.size}, authored=${plan.ownOutboundFrames.size}, receipts=${plan.ownReceiptFrames.size})",
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to build digest spray plan for $address: ${e.message}")
        }
    }
}
