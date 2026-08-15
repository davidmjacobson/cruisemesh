package com.cruisemesh.app.mesh

import android.content.Context
import android.util.Log
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.friending.FriendDirectorySender
import com.cruisemesh.app.friending.FriendImportEvents
import com.cruisemesh.app.friending.FriendRequestSender
import com.cruisemesh.app.friending.FriendDirectoryScope
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.friending.ProfileSyncSender
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.KIND_REACTION
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.IncomingMessageAnnouncer
import com.cruisemesh.app.notify.NotificationAnnouncer
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayFetchedEnvelope
import com.cruisemesh.app.relay.RelayImport
import uniffi.cruisemesh_core.CarriedEnvelope
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.ContactDiscoveryPolicy
import uniffi.cruisemesh_core.ContactProvenance
import uniffi.cruisemesh_core.CoreCarriedOfferReservation
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreInboundDisposition
import uniffi.cruisemesh_core.CoreInboundGate
import uniffi.cruisemesh_core.CoreSprayGate
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.IncomingMessageInsertOutcome
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.PeerConnectionEventKind
import uniffi.cruisemesh_core.MessageBody
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.OpenedMessage
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.ReceiptContent
import uniffi.cruisemesh_core.PendingSharedRequest
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.applyGroupMetadataUpdate
import uniffi.cruisemesh_core.coreCarriedPageMaxRows
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.coreInboundGate
import uniffi.cruisemesh_core.coreIsOwnFanoutHint
import uniffi.cruisemesh_core.corePairwiseSenderAuthorized
import uniffi.cruisemesh_core.corePeerTransportForArrival
import uniffi.cruisemesh_core.decodeExtendedMessageBody
import uniffi.cruisemesh_core.decodeFriendDirectoryContent
import uniffi.cruisemesh_core.decodeGroupInviteContent
import uniffi.cruisemesh_core.decodeGroupMetadataUpdate
import uniffi.cruisemesh_core.decodeIntroducedFriendRequest
import uniffi.cruisemesh_core.decodeLanEndpointContent
import uniffi.cruisemesh_core.decodeProfileSyncContent
import uniffi.cruisemesh_core.decodeReceiptContent
import uniffi.cruisemesh_core.decodeRelayUpdateContent
import uniffi.cruisemesh_core.encodeEnvelopeFrame
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.openGroupMessage
import uniffi.cruisemesh_core.openMessage
import uniffi.cruisemesh_core.parseFriendCard
import uniffi.cruisemesh_core.parseFriendRequestContent
import uniffi.cruisemesh_core.recentHintsFor
import uniffi.cruisemesh_core.verifyIntroductionTicket
import uniffi.cruisemesh_core.verifySharedFriendCard

// Deliberately MeshService's tag, not this class's name: this code moved here
// verbatim in the FA15 extraction, and field tooling (logcat filters, the
// debug-report scripts) matches on the "MeshService" tag for delivery lines.
private const val TAG = "MeshService"

/** DESIGN.md §5.3: the bounded budget (~5 MB) of *foreign* muled envelopes; family (known-recipient) traffic is exempt. */
private const val FOREIGN_CARRY_BUDGET_BYTES: Long = 5L * 1024 * 1024

// The three per-encounter spray byte budgets that used to live here -- carried
// 256 KiB, own outbound 256 KiB, receipts 64 KiB -- are gone. iOS carried the
// same three numbers in ProtocolKinds.swift; they were equal, and nothing made
// them stay equal. They now live in `core/src/spray_policy.rs` beside the plan
// that spends them, and arrive at these call sites inside a `CoreSprayGate`
// already scaled by what the link can take (#280, ledger row SPRAY-01).
//
// [FOREIGN_CARRY_BUDGET_BYTES] above is deliberately NOT one of them: it
// bounds how much foreign traffic this device *stores*, not how much it offers
// in one encounter.

/**
 * G3: the shared foreign-carry allowance, whose cap and window both live in the
 * core (see `CoreCarriedOfferGate`). Shared by the HELLO drain and the digest
 * spray so the two lanes cannot each claim a full allowance for one peer.
 */
private val carriedOfferGate = CarriedOfferEpochGate()

/**
 * Decoded pairwise-stream metadata carried out of the delivery switch only
 * after the handler has reached a terminal consumed state.
 *
 * [recordStreamLamport] is false for malformed/unauthorized stream metadata:
 * those envelopes remain terminally consumed for relay-ack purposes, but
 * cannot be allowed to close a legitimate chat gap.
 */
private data class PairwiseDeliveryResult(
    val kind: UByte,
    val senderUserId: ByteArray,
    val lamport: ULong,
    val recordStreamLamport: Boolean,
)

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
    /**
     * Where "tell the user something arrived" goes. Defaults to the real
     * notification path, so production callers (MeshService) need no change;
     * tests substitute a recording fake to assert the ROADMAP notification
     * release gate. See [IncomingMessageAnnouncer] for why this is injectable
     * at all -- before it, the gate's decisive branch was the one branch of
     * this class no unit test could execute.
     */
    private val announcer: IncomingMessageAnnouncer = NotificationAnnouncer(context),
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
        /**
         * Files an address a contact claimed in a hint. Unproven by
         * definition -- nothing here has reached it -- so it is cached as
         * [uniffi.cruisemesh_core.LanEndpointProvenance.HINTED] and stays
         * subject to the same-network rule every time it is read back.
         */
        fun saveHintedLanEndpoint(networkId: String?, userId: ByteArray, endpoint: LanManualEndpoint)
        fun currentLanNetworkId(): String?

        /**
         * A contact just demonstrated LAN support, so the automatic-scan
         * gate's cached capability set is stale.
         */
        fun onLanCapabilityChanged()
    }

    /** FA5: atomic per-msg_id admission gate across the four concurrent receive-path threads -- see [processInboundEnvelope]. */
    private val inboundAdmission = InboundEnvelopeAdmission()

    /**
     * The core receive engine, selected per envelope by
     * [InboundEngineSettings]. Its [CoreInboundAdapter.Intents] are the three
     * things that stay native once core owns the disposition: putting the
     * flood copy on the wire, durably delivering an opened payload through the
     * per-kind handlers below, and nudging the relay upload lane when core
     * classified a carry as family.
     */
    private val coreInbound = CoreInboundAdapter(
        store,
        GossipState.seenIds,
        object : CoreInboundAdapter.Intents {
            override fun flood(sourceAddress: String?, frame: ByteArray) {
                val fanout = if (sourceAddress == null) {
                    MeshRouter.relayToAll(frame)
                } else {
                    MeshRouter.relayToAllExcept(sourceAddress, frame)
                }
                if (fanout > 0) {
                    Log.i(TAG, "Relayed foreign envelope from ${sourceAddress ?: "relay"} to $fanout link(s)")
                }
            }

            override fun deliver(
                sourceAddress: String?,
                hopTtl: UByte,
                msgId: ByteArray,
                senderUserId: ByteArray,
                groupId: ByteArray?,
                payload: ByteArray,
                identity: Identity,
            ): Boolean {
                val sourceLabel = sourceAddress ?: "relay"
                val opened = OpenedMessage(senderUserId = senderUserId, payload = payload)
                val arrival = messageArrival(sourceAddress, hopTtl, senderUserId)
                return try {
                    if (groupId != null) {
                        val group = store.getGroup(groupId)
                        if (group == null) {
                            // Core opened this body with that group's key, so
                            // the row disappearing between the open and here
                            // is a store problem, not a delivery verdict:
                            // report failure and leave the envelope
                            // re-presentable rather than acking it away.
                            Log.w(TAG, "Deferring group envelope from $sourceLabel: group row is gone")
                            return false
                        }
                        deliverOpenedGroupEnvelope(sourceLabel, group, opened, identity, arrival, msgId)
                    } else {
                        val consumed = deliverOpenedEnvelope(
                            sourceLabel,
                            sourceAddress != null,
                            opened,
                            identity,
                            arrival,
                            msgId,
                        )
                        // The ACK-01 msg_id evidence rides core's commit
                        // token; only the gap-rendering lamport is left here.
                        if (consumed != null) {
                            recordConsumedStreamLamport(consumed)
                        }
                    }
                    true
                } catch (e: CoreException) {
                    // T4-06: a message that was OURS to open failed to persist.
                    // Never let it unwind the receive thread.
                    Log.w(TAG, "Deferring envelope from $sourceLabel: durable delivery failed (${e.message})")
                    false
                }
            }

            override fun onFamilyCarry() {
                requestRelaySync("family carry queued")
            }
        },
    )

    /**
     * DESIGN.md §7.3: receipts go first on peer sync because they're the
     * smallest frames and unblock the most UI. The store persists the latest
     * cumulative delivered/read watermarks we owe [contact], so a receipt that
     * couldn't be sent when it was first observed heals on this reconnect.
     *
     * The peer's digest is deliberately not consulted -- see
     * [ReceiptRepair.owedTo] for why capping these watermarks against it
     * self-locked the pairing.
     */
    /**
     * Sends every receipt this device still owes [contact] on [address].
     *
     * Returns the sealed bytes queued, so the caller can charge them against
     * the link's burst allowance: this pass is one of the encounter's lanes
     * that no spray plan can see (#280).
     */
    fun syncReceiptsFirst(
        identity: Identity,
        contact: Contact,
        address: String,
    ): Long {
        var queuedBytes = 0L
        for (owed in ReceiptRepair.owedTo(store, contact.userId)) {
            queuedBytes += sendReceiptOnAddress(
                identity,
                contact,
                address,
                owed.receiptType,
                contact.userId,
                owed.throughLamport,
            )
        }
        return queuedBytes
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
        // The rollout switch. Read once, here, before any disposition work
        // starts: an envelope runs entirely on one engine or entirely on the
        // other. Core records the seen-set entry itself, inside the call, so
        // the claim above is released with terminal = false -- the record has
        // already landed under this instance's lock by the time `finish` runs,
        // which is the same guarantee the legacy returns get from the
        // terminal = true hook.
        if (InboundEngineSettings.inboundEngine(context) == InboundEngine.CORE) {
            return try {
                coreInbound.process(sourceAddress, envelope, identity)
            } finally {
                inboundAdmission.finish(envelope.msgId, terminal = false) {}
            }
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
            val groupOpened = tryOpenGroupMessage(envelope.recipientHint, identity.userId, envelope.sealed)
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
        val consumed = try {
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
        // The ONE place this device may vouch for a hidden kind's relay copy:
        // reaching here means [openMessage] succeeded against our own identity
        // key (so the envelope was pairwise-sealed to us and nobody else can
        // open it) and delivery ran to completion. Both consumption paths pass
        // through here -- BLE/LAN frames and [handleRelayEnvelope] alike -- so
        // a relay-consumed hidden kind is equally re-ackable if the mailbox
        // ever re-presents it. See
        // [MessageStore.coreRecordConsumedHiddenMsgId] for every condition
        // core re-checks and for why anything unprovable must not be recorded.
        if (consumed != null) {
            recordConsumedHiddenKind(envelope, consumed, identity)
        }
        // DTN D4: reaching here means the message was durably stored -- safe,
        // and required, to record.
        return finishAdmission(CoreInboundDisposition.CONSUMED, terminal = true)
    }

    /**
     * Best-effort note that this device consumed [envelope] as its sole true
     * endpoint consumer, so a later relay copy of the same `msg_id` can be
     * acked away instead of sitting in the mailbox until expiry.
     *
     * Deliberately swallows store failures: a missing record costs one relay
     * re-fetch, which is precisely the cost this mechanism trades against,
     * and must never turn into a failed delivery. Core owns every safety
     * condition (kind, own-hint, group-hint, expiry) and simply declines to
     * write a row when one doesn't hold, so this call site's only job is to
     * be reached exclusively from the proven-consumption path above.
     *
     * The same terminal hook records an exact, validated pairwise lamport for
     * gap rendering when the handler left no message row. Core accepts that
     * longer-lived evidence only for an established contact and an actual 1:1
     * stream, so stranger onboarding traffic cannot grow it indefinitely.
     */
    private fun recordConsumedHiddenKind(
        envelope: Frame.Envelope,
        consumed: PairwiseDeliveryResult,
        identity: Identity,
    ) {
        try {
            store.coreRecordConsumedHiddenMsgId(
                envelope.msgId,
                consumed.kind,
                envelope.recipientHint,
                envelope.expiry,
                identity.userId,
                System.currentTimeMillis(),
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to record consumed hidden-kind msg_id: ${e.message}")
        }
        recordConsumedStreamLamport(consumed)
    }

    /**
     * The gap-rendering half of [recordConsumedHiddenKind], split out because
     * the core engine's commit token covers the ACK-01 msg_id evidence but not
     * this: an exact, validated pairwise lamport recorded when the handler left
     * no message row. Core accepts that longer-lived evidence only for an
     * established contact and an actual 1:1 stream, so stranger onboarding
     * traffic cannot grow it indefinitely.
     */
    private fun recordConsumedStreamLamport(consumed: PairwiseDeliveryResult) {
        if (!consumed.recordStreamLamport) return
        try {
            if (
                store.recordConsumedHiddenLamport(
                    consumed.senderUserId,
                    consumed.senderUserId,
                    consumed.lamport,
                    consumed.kind,
                )
            ) {
                ChatEvents.notifyChatChanged(consumed.senderUserId)
            }
        } catch (e: CoreException) {
            // Gap evidence is explanatory metadata, not delivery durability:
            // failing to write it must not turn an already-handled control
            // envelope into an unbounded retry loop.
            Log.w(TAG, "Failed to record consumed hidden-kind lamport: ${e.message}")
        }
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
     * Opens [sealed] with any imported group [MessageStore.groupOpenCandidates]
     * offers for [recipientHint]: groups whose own recent-day hints match,
     * plus every imported group when the hint is OUR OWN -- a per-member
     * relay fan-out copy (specs/group-relay-durability.md §4.1) is addressed
     * to the member, not the group, so nothing but the group key identifies
     * it. Returns the matching [Group] and opened payload, or null.
     * [openGroupMessage] does not check membership of the signer; callers
     * must enforce that before trusting the body.
     */
    private fun tryOpenGroupMessage(
        recipientHint: ByteArray,
        ownUserId: ByteArray,
        sealed: ByteArray,
    ): Pair<Group, OpenedMessage>? {
        val now = System.currentTimeMillis()
        for (group in store.groupOpenCandidates(recipientHint, ownUserId, now)) {
            try {
                return group to openGroupMessage(group, sealed)
            } catch (_: CoreException) {
                // Wrong key / corrupt — try the next candidate group.
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
    fun drainCarriedEnvelopesTo(address: String, peerUserId: ByteArray, carriedBudgetBytes: ULong): Long {
        if (carriedBudgetBytes == 0uL) {
            Log.d(TAG, "Targeted carried drain skipped for $address (no carried budget this encounter)")
            return 0L
        }
        val now = System.currentTimeMillis()
        var carriedReservation: CoreCarriedOfferReservation? = null
        var queuedBytes = 0L
        try {
            store.pruneExpiredCarried(now)
            // G2: budgeted page + resume cursor (same DTN rules — offer only).
            val lane = MeshRouter.targetedCarriedLaneFor(address, now)
            if (lane.skip) {
                Log.d(TAG, "Targeted carried drain parked for $address (rewalk cooldown)")
                return 0L
            }
            // HELLO drains share G3's global allowance with digest sprays and
            // reserve by authenticated user. Duplicate BLE roles or rotating
            // addresses for one phone cannot each enqueue a full page in the
            // same connection burst.
            carriedReservation = carriedOfferGate.tryReserve(now, UserIdHex.encode(peerUserId))
            if (carriedReservation == null) {
                Log.d(TAG, "Targeted carried drain deferred for $address (logical-peer/global cap)")
                return 0L
            }
            val reservation = carriedReservation
            // Peer userId hints plus every group that peer is a member of
            // (DESIGN.md §6.5: members mule for the whole group).
            val deliveryHints = store.deliveryHintsForPeer(peerUserId, now)
            val page = store.carriedEnvelopesForHintsPage(
                deliveryHints,
                now,
                carriedBudgetBytes,
                coreCarriedPageMaxRows(),
                lane.after,
            )
            if (page.rows.isEmpty()) {
                carriedOfferGate.release(reservation)
                carriedReservation = null
                MeshRouter.recordTargetedCarriedProgress(address, page.next, page.exhausted, now)
                return 0L
            }
            carriedOfferGate.commit(reservation)
            carriedReservation = null
            var delivered = 0
            for (env in page.rows) {
                val frame = encodeEnvelopeFrame(env.msgId, env.hopTtl, env.expiry, env.recipientHint, env.sealed)
                if (MeshRouter.sendToAddress(address, frame)) {
                    delivered++
                    // Charged against the link's burst allowance by the caller:
                    // this drain is one of the encounter's largest lanes and is
                    // not part of any spray plan (#280).
                    queuedBytes += env.sealed.size.toLong()
                }
            }
            // Never remove carried on send — digest proof only.
            MeshRouter.recordTargetedCarriedProgress(address, page.next, page.exhausted, now)
            Log.i(
                TAG,
                "Attempted delivery of $delivered/${page.rows.size} carried envelope(s) to $address " +
                    "(budgeted HELLO drain; exhausted=${page.exhausted}; removal awaits their digest confirmation)",
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to drain carried envelopes to $address: ${e.message}")
        } finally {
            carriedReservation?.let(carriedOfferGate::release)
        }
        return queuedBytes
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
     *
     * Returns the body's stream metadata once it is known, or `null` if the
     * body could not even be decoded. Every other early return still reports
     * its kind for relay-ack evidence, but marks invalid/unauthorized stream
     * metadata as unable to close a chat gap:
     * a deliberate discard (blocked sender, unauthorized sender, unhandled
     * kind) is consumption by an endpoint that is finished with the envelope,
     * which is exactly what [processInboundEnvelope] treats as CONSUMED and
     * what may be recorded as a consumed hidden kind. Only "we could not tell
     * what this was" withholds that.
     */
    private fun deliverOpenedEnvelope(
        address: String,
        directBle: Boolean,
        opened: OpenedMessage,
        identity: Identity,
        arrival: MessageArrival,
        msgId: ByteArray,
    ): PairwiseDeliveryResult? {
        val extendedBody = try {
            decodeExtendedMessageBody(opened.payload)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping envelope from $address: failed to decode body (${e.message})")
            return null
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
            return PairwiseDeliveryResult(body.kind, opened.senderUserId, body.lamport, false)
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
            return PairwiseDeliveryResult(body.kind, opened.senderUserId, body.lamport, false)
        }

        // Blocked identities are dropped before ANY kind handler runs: a
        // replayed kind=3 must not resurrect the contact, no receipts are
        // authored (the blocked party sees nothing), and the relay copy still
        // acks away as consumed — we are the sole endpoint and deliberate
        // discard is consumption, so the mailbox doesn't refetch it forever.
        if (store.isUserBlocked(opened.senderUserId)) {
            Log.i(TAG, "Dropping envelope from $address: sender is blocked")
            return PairwiseDeliveryResult(body.kind, opened.senderUserId, body.lamport, false)
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
            KIND_RELAY_UPDATE -> handleIncomingRelayUpdate(
                address,
                opened.senderUserId,
                body,
                identity,
            )
            else -> Log.i(TAG, "Dropping envelope from $address: unhandled kind=${body.kind}")
        }
        return PairwiseDeliveryResult(body.kind, opened.senderUserId, body.lamport, true)
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
        LanCapabilityStore.markSupported(context, senderUserId)
        lan.onLanCapabilityChanged()
        val now = System.currentTimeMillis()
        // A sealed hint is only good for its stated lifetime (fifteen minutes;
        // see LanEndpointSender). This envelope may have sat in a relay
        // backlog for hours or days, so an expired hint is neither saved nor
        // dialed -- saving first would re-file a long-dead address and reset
        // its seven-day cache clock on every replay.
        if (content.expiresAtMs > now) {
            lan.saveHintedLanEndpoint(hintedNetworkId, senderUserId, endpoint)
            // The network fingerprint is stored with the cached endpoint but
            // deliberately does NOT gate this dial: requiring an exact match
            // silently disabled fresh hints on routed multi-subnet LANs -- the
            // case the sealed hint exists for (mDNS is link-local; TCP may
            // still route). A cross-network false positive is one bounded TCP
            // attempt to an endpoint the contact sealed to us, and Noise
            // authenticates. Being on some Wi-Fi is the only requirement.
            if (lan.currentLanNetworkId() != null) {
                lan.connectToLanHint(
                    Frame.LanEndpoint(
                        instanceToken = content.instanceToken,
                        host = content.host,
                        port = content.port,
                    ),
                    senderUserId,
                )
            }
        }
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
    }

    /**
     * Delivers a group-sealed envelope we opened with an imported group key
     * (DESIGN.md §6.5). Wire [MessageBody.chatId] is the group id; the
     * verified signer must be a current member (core does not check this).
     * D9 group receipts go pairwise back to the author after the local
     * watermark advances.
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
        if (store.isUserBlocked(opened.senderUserId)) {
            Log.i(TAG, "Dropping group envelope from $address: sender is blocked")
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
                identity,
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

    private fun acceptIncomingInsert(
        outcome: IncomingMessageInsertOutcome,
        address: String,
        kind: UByte,
        senderUserId: ByteArray,
        lamport: ULong,
    ): Boolean = when (outcome) {
        IncomingMessageInsertOutcome.INSERTED -> true
        IncomingMessageInsertOutcome.DUPLICATE -> {
            Log.i(
                TAG,
                "Ignoring duplicate kind=$kind from $address " +
                    "sender=${UserIdHex.encode(senderUserId)} lamport=$lamport",
            )
            false
        }
        IncomingMessageInsertOutcome.QUARANTINED_CONFLICT -> {
            Log.w(
                TAG,
                "Quarantined message stream conflict kind=$kind from $address " +
                    "sender=${UserIdHex.encode(senderUserId)} lamport=$lamport; retained visible branch",
            )
            ChatEvents.notifyChatChanged(senderUserId)
            false
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
        val outcome = store.insertIncomingMessageWithArrival(
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
            arrival,
        )
        if (!acceptIncomingInsert(outcome, address, body.kind, senderUserId, body.lamport)) return
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
        identity: Identity,
    ) {
        val outcome = store.insertIncomingMessageWithArrival(
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
            arrival,
        )
        if (!acceptIncomingInsert(outcome, address, body.kind, senderUserId, body.lamport)) return
        Log.i(
            TAG,
            "Stored group kind=${body.kind} in ${group.name} from $address " +
                "sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
        recordInboundChatArrival(senderUserId, body.kind, arrival)
        ChatEvents.notifyChatChanged(group.id)

        // See [PeerStreamWatermark] for why this is a plain MAX.
        val throughLamport = PeerStreamWatermark.through(store, group.id, senderUserId)
        store.recordOutgoingReceipt(group.id, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        val isVisible = ChatVisibility.isVisible(group.id)
        if (isVisible) {
            store.recordOutgoingReceipt(group.id, senderUserId, RECEIPT_TYPE_READ, throughLamport)
        }
        emitGroupReceiptsToAuthor(group, senderUserId, identity)
        if (!isVisible && isVisibleChatKind(body.kind)) {
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
            announcer.announceGroupMessage(group, senderName, preview)
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
            // The invite rides the 1:1 pairwise lamport stream, so it must be
            // acknowledged on that stream like any other pairwise kind -- even
            // though its row lives under the group chat. Skipping the ack (as
            // this did) strands the peer's delivered watermark below the
            // invite's lamport for as long as the invite is the newest thing
            // they sent us, and the repair lane can never lift it, so they
            // replay their backlog on every send. DELIVERED only, like every
            // other row that never appears in the 1:1 chat.
            acknowledgePeerStream(
                identity,
                contact,
                address,
                senderUserId,
                markRead = false,
                atLeastLamport = body.lamport,
            )
        }
        if (!ChatVisibility.isVisible(group.id)) {
            // FA8: a typed entry point, not a literal string sniffed by
            // MessageNotifier's prefix check -- see notifyGroupInvite's KDoc.
            announcer.announceGroupInvite(group)
        }
    }

    /**
     * Was this peer in range when we accepted them? Recorded in
     * [ContactProvenance.addedNearby] so the composer can stay quiet about
     * nearby-only delivery for people we actually met, and say it plainly for
     * people who only ever arrived over the internet.
     *
     * A direct BLE arrival counts on its own: the envelope came off a link to
     * their phone, which is the strongest evidence of range there is. The
     * nearby set covers the LAN case (and a BLE peer whose HELLO landed under
     * a different address).
     */
    private fun peerIsNearby(senderUserId: ByteArray, directBle: Boolean): Boolean =
        directBle || MeshConnectivityStatus.nearbyPeerIds.value.contains(UserIdHex.encode(senderUserId))

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
        val content = try {
            parseFriendRequestContent(body.content.toString(Charsets.UTF_8))
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping friend request from $address: failed to parse FriendCard (${e.message})")
            return
        }
        val card = content.card
        if (!friendCardUserId(card).contentEquals(senderUserId)) {
            Log.w(TAG, "Dropping friend request from $address: payload identity doesn't match verified sender")
            return
        }
        // A tail means this request came from a card somebody shared, and the
        // whole point of the marker is that it does NOT auto-import.
        content.shared?.let { shared ->
            holdSharedFriendRequest(address, senderUserId, body, identity, card, shared)
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
                addedNearby = peerIsNearby(senderUserId, directBle),
            ),
        )
        if (pendingSuggestion != null) store.removeFriendSuggestion(senderUserId)
        // Their mutual request is the only answer a shared-card import ever
        // gets, so it is what ends this side's "waiting" state.
        store.deleteOutgoingSharedRequest(senderUserId)
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

        acknowledgePeerStream(identity, contact, address, senderUserId, markRead = ChatVisibility.isVisible(senderUserId))
        if (!wasKnown) {
            FriendImportEvents.notifyImported(contact, directBle)
            announcer.announceFriendAdded(contact)
        }
        Log.i(TAG, "Imported contact ${contact.name} from friend request on $address")
    }

    /**
     * A `kind=3` carrying a shared-card tail (specs/share-contact.md decision
     * 5): nothing may touch `contacts` until this user says yes, so the request
     * parks in `pending_shared_requests` and the only user-visible effect is a
     * rate-limited notification.
     *
     * Every check below drops the request without a prompt, deliberately: a
     * failure here is either somebody else's expired artifact or an attempt to
     * get in, and neither is worth a question the user cannot evaluate. The
     * envelope is still acked and stored in the hidden stream so the requester
     * stops re-spraying it -- silence about the *decision* is not silence about
     * receipt, and the alternative is an endless resend loop for a request
     * whose answer is "no".
     */
    private fun holdSharedFriendRequest(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
        card: uniffi.cruisemesh_core.FriendCard,
        shared: uniffi.cruisemesh_core.SharedFriendCard,
    ) {
        // The requester themselves is already gated upstream (blocked senders
        // never reach any kind handler); the sharer is the one nobody has
        // checked yet.
        val sharer = store.getContact(shared.sharerUserId)
        if (sharer == null) {
            Log.i(TAG, "Dropping shared friend request from $address: sharer is not a contact")
            return
        }
        if (store.isUserBlocked(shared.sharerUserId)) {
            Log.i(TAG, "Dropping shared friend request from $address: sharer is blocked")
            return
        }
        if (!friendCardUserId(shared.card).contentEquals(identity.userId)) {
            Log.w(TAG, "Dropping shared friend request from $address: the shared card is not ours")
            return
        }
        if (!FriendsOfFriendsStore.isEnabled(context)) {
            Log.i(TAG, "Dropping shared friend request from $address: introductions are off")
            return
        }
        val now = System.currentTimeMillis()
        val valid = try {
            verifySharedFriendCard(
                shared,
                sharer.signPk,
                identity.userId,
                FriendsOfFriendsStore.revision(context),
                now,
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping shared friend request from $address: ${e.message}")
            return
        }
        if (!valid) {
            Log.w(TAG, "Dropping shared friend request from $address: shared card failed validation")
            return
        }
        if (store.getSharedRequestDismissal(senderUserId)?.suppressed == true) {
            Log.i(TAG, "Dropping shared friend request from $address: requester was suppressed")
            return
        }

        store.upsertPendingSharedRequest(
            PendingSharedRequest(
                requesterUserId = senderUserId,
                name = card.name,
                signPk = card.signPk,
                agreePk = card.agreePk,
                relayUrl = card.relayUrl,
                relayToken = card.relayToken,
                sharerUserId = shared.sharerUserId,
                expiresAtMs = shared.expiresAtMs,
                firstSeenMs = now,
                lastPromptedMs = 0L,
            ),
        )
        store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_FRIEND_REQUEST,
                payload = body.content,
            ),
        )
        // Not a contact, so the receipt is sealed to the card in the request
        // itself. Nothing about this transient is persisted.
        acknowledgeHiddenMessage(
            address,
            senderUserId,
            identity,
            Contact(
                userId = senderUserId,
                name = card.name,
                signPk = card.signPk,
                agreePk = card.agreePk,
                relayUrl = card.relayUrl,
                relayToken = card.relayToken,
            ),
        )
        ChatEvents.notifyChatChanged(senderUserId)
        if (store.noteSharedRequestPrompt(senderUserId, now)) {
            announcer.announceSharedRequest(senderUserId, card.name)
        }
        Log.i(TAG, "Holding shared friend request from $address for confirmation")
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
        acknowledgePeerStream(identity, contact, address, senderUserId, markRead = ChatVisibility.isVisible(senderUserId))
        if (policyChanged) {
            FriendDirectorySender.queueToAllContacts(context, store, identity)
        }
    }

    /**
     * T23: a contact told us their own relay endpoint changed.
     *
     * `opened.senderUserId` is the identity core verified sealed this
     * envelope, and it is the only user id passed to
     * `applyContactRelayUpdate` — core rejects the notice outright if its
     * payload claims a different subject, so a notice can only ever move its
     * own sender's endpoint, never a third party's.
     */
    private fun handleIncomingRelayUpdate(
        address: String,
        senderUserId: ByteArray,
        body: MessageBody,
        identity: Identity,
    ) {
        val contact = store.getContact(senderUserId) ?: run {
            Log.i(TAG, "Dropping relay update from $address: sender is not a contact")
            return
        }
        val content = try {
            decodeRelayUpdateContent(body.content)
        } catch (e: CoreException) {
            Log.w(TAG, "Dropping relay update from $address: failed to decode (${e.message})")
            return
        }
        val inserted = store.insertMessage(
            StoredMessage(
                chatId = senderUserId,
                senderUserId = senderUserId,
                lamport = body.lamport,
                timestamp = body.timestamp,
                kind = KIND_RELAY_UPDATE,
                payload = body.content,
            ),
        )
        if (!inserted) return

        val applied = try {
            store.applyContactRelayUpdate(senderUserId, content)
        } catch (e: CoreException) {
            // Mis-scoped subject or a non-deposit credential: a deterministic
            // reject, not a store failure. The message row above still stands
            // so the sender's watermark advances and they stop re-spraying it.
            Log.w(TAG, "Rejecting relay update from $address: ${e.message}")
            false
        }
        if (applied) {
            Log.i(TAG, "Applied relay update from ${UserIdHex.encode(senderUserId)}")
            // Anything queued for them was addressed to the old endpoint's
            // mailbox; a sync pass re-resolves and re-posts to the new one.
            RelaySyncEvents.requestSync()
            ChatEvents.notifyChatChanged(senderUserId)
        }
        acknowledgeHiddenMessage(address, senderUserId, identity, contact)
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
            // Introductions stay inside one Shore Pass. A directory from an
            // introducer on somebody else's pass is applied as an *empty*
            // snapshot rather than ignored: the revision bookkeeping stays
            // identical, and it additionally clears whatever that introducer
            // supplied before this rule existed. A phone therefore heals on
            // its own next pass instead of waiting for every other phone in
            // the graph to update.
            val ownRelay = RelayConfigStore.load(context)
            val scoped = if (
                FriendDirectoryScope.introducible(contact, ownRelay?.relayUrl, ownRelay?.relayToken)
            ) {
                content
            } else {
                Log.i(TAG, "Scoping out friend directory from $address: introducer is on another pass")
                content.copy(entries = emptyList())
            }
            try {
                if (store.applyFriendDirectory(senderUserId, identity.userId, scoped, System.currentTimeMillis())) {
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
                addedNearby = peerIsNearby(senderUserId, directBle),
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
            announcer.announceFriendAdded(contact)
        }
    }

    /** Hidden-kind rows (endpoint hints, directories, introductions) are never on screen, so they ack DELIVERED only -- never READ. */
    private fun acknowledgeHiddenMessage(
        address: String,
        senderUserId: ByteArray,
        identity: Identity,
        contact: Contact,
    ) = acknowledgePeerStream(identity, contact, address, senderUserId, markRead = false)

    /**
     * The receipt handshake every consumed inbound peer-stream message ends
     * with, in one place (FA15 follow-up -- this was spelled out verbatim in
     * the friend-request, profile-sync, and hidden-message handlers, and D9's
     * group receipts will extend exactly this sequence): persist the
     * cumulative DELIVERED watermark (plus READ when [markRead] -- the chat
     * is on screen), refresh the relay-uploadable receipt envelope(s), kick a
     * relay sync if a watermark advanced, and send the receipt(s) back on the
     * link the message arrived on.
     *
     * See [PeerStreamWatermark] for why the watermark is a plain MAX and what
     * [atLeastLamport] is for (the group invite, whose row lives under the
     * group chat but whose lamport belongs to this 1:1 stream).
     */
    private fun acknowledgePeerStream(
        identity: Identity,
        contact: Contact,
        address: String,
        senderUserId: ByteArray,
        markRead: Boolean,
        atLeastLamport: ULong = 0uL,
    ) {
        val throughLamport = PeerStreamWatermark.through(store, senderUserId, senderUserId, atLeastLamport)
        store.recordOutgoingReceipt(senderUserId, senderUserId, RECEIPT_TYPE_DELIVERED, throughLamport)
        var relayQueueChanged = queueOutgoingReceiptForRelay(
            identity = identity,
            contact = contact,
            receiptType = RECEIPT_TYPE_DELIVERED,
            ackedSenderUserId = senderUserId,
            throughLamport = throughLamport,
        )
        if (markRead) {
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
        if (markRead) {
            sendReceiptOnAddress(identity, contact, address, RECEIPT_TYPE_READ, senderUserId, throughLamport)
        }
    }

    /**
     * Stores an incoming text message and, only if it was newly inserted,
     * sends a delivered receipt back on the same link (DESIGN.md §7.2), plus
     * -- if the chat is currently on screen ([ChatVisibility.isVisible]) -- a
     * read receipt too. Otherwise, posts a notification
     * ([IncomingMessageAnnouncer.announceDirectMessage]) instead, since the chat isn't
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
        val outcome = store.insertIncomingMessageWithArrival(
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
            arrival,
        )
        if (!acceptIncomingInsert(outcome, address, kind, senderUserId, body.lamport)) return
        Log.i(
            TAG,
            "Stored kind=$kind from $address sender=${UserIdHex.encode(senderUserId)} lamport=${body.lamport}",
        )
        MeshConnectivityStatus.mergeLastSeen(UserIdHex.encode(senderUserId), System.currentTimeMillis())
        ChatEvents.notifyChatChanged(senderUserId)

        // See [PeerStreamWatermark] for why this is a plain MAX and not the
        // contiguous count.
        val throughLamport = PeerStreamWatermark.through(store, senderUserId, senderUserId)
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
        recordInboundChatArrival(senderUserId, kind, arrival)

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
            announcer.announceDirectMessage(contact, preview)
        }
    }

    /**
     * Records that a friend's own message landed on this phone, for the
     * Connection details screen.
     *
     * Deliberately narrow. Only kinds a person actually sees in a
     * conversation count ([isVisibleChatKind]) -- receipts, profile sync,
     * relay updates, endpoint hints, reactions and every other hidden kind
     * are machine chatter and would make the screen claim a friend had
     * written when nobody did. Unknown senders are skipped too: the screen
     * only lists friends, so an event for anyone else could never be shown
     * against a name.
     *
     * Best-effort: connection history is a diagnostic, never worth failing a
     * real message delivery over.
     */
    private fun recordInboundChatArrival(
        senderUserId: ByteArray,
        kind: UByte,
        arrival: MessageArrival,
    ) {
        if (!isVisibleChatKind(kind)) return
        // Everything here is best-effort, the contact lookup included. This
        // runs after the message row is already committed, so letting a store
        // error escape would abandon the receipt and notification that follow
        // it -- and the retry hits the duplicate early-return, stranding them
        // for good. Connection history is diagnostics; it must never cost a
        // message its delivery path. Swift's `try?` covers the same two calls.
        runCatching {
            if (store.getContact(senderUserId) == null) return
            store.recordPeerConnectionEvent(
                senderUserId,
                corePeerTransportForArrival(arrival.transport),
                PeerConnectionEventKind.MESSAGE_RECEIVED,
                arrival.receivedAt,
            )
        }.onFailure { error ->
            Log.w(TAG, "Could not record inbound arrival in connection history: ${error.message}")
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
        val groupId = receipt.groupId
        if (groupId != null) {
            handleIncomingGroupReceipt(
                address,
                envelopeSenderUserId,
                receipt,
                groupId,
                identity,
                arrival,
            )
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
        // A receipt is the other half of what the receipt-quiet backoff (#280)
        // watches for: sprays toward this peer are converging, so its cadence
        // returns to normal.
        SprayPolicy.noteReceiptProgress(envelopeSenderUserId)
        // The receipt returned on the exact link that delivered the message;
        // record that route against the watermark (T6) so every acknowledged
        // message's Info pane can prove LAN/BLE/relay delivery -- not just the
        // one at the exact watermark lamport.
        //
        // `receivedAtMs` also lets core record the MESSAGE_DELIVERED half of
        // the Connection details evidence, which is the only place it is
        // recorded. This shell used to write that event itself on every
        // delivered receipt, which made the screen claim a friend had received
        // a message when all their phone had acked was a profile-sync or
        // friend-directory blob. Core records it only when the watermark newly
        // covers a visible message we authored.
        store.recordReceipt(
            chatId = envelopeSenderUserId, // local convention: chat keyed by the other party -- see class KDoc
            senderUserId = identity.userId, // whose messages this receipt is about: ours
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
            viaTransport = arrival.transport,
            receivedAtMs = arrival.receivedAt,
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
     * A pairwise-sealed D9 group receipt: [envelopeSenderUserId] is the member
     * acking our authored stream in [groupId]. Isolated from the 1:1
     * `receipts` table so it cannot paint ticks on a pairwise chat.
     */
    private fun handleIncomingGroupReceipt(
        address: String,
        envelopeSenderUserId: ByteArray,
        receipt: ReceiptContent,
        groupId: ByteArray,
        identity: Identity,
        arrival: MessageArrival,
    ) {
        if (!receipt.senderUserId.contentEquals(identity.userId)) {
            Log.w(
                TAG,
                "Dropping group receipt from $address: acks senderUserId=${UserIdHex.encode(receipt.senderUserId)}, not us",
            )
            return
        }
        val group = store.getGroup(groupId)
        if (group == null) {
            Log.w(TAG, "Dropping group receipt from $address: unknown group")
            return
        }
        if (!group.memberUserIds.any { it.contentEquals(envelopeSenderUserId) } ||
            !group.memberUserIds.any { it.contentEquals(identity.userId) }
        ) {
            Log.w(TAG, "Dropping group receipt from $address: sender or we are not a member")
            return
        }
        if (store.getContact(envelopeSenderUserId) == null) {
            Log.w(TAG, "Dropping group receipt from $address: envelope sender is not a known contact")
            return
        }
        SprayPolicy.noteReceiptProgress(envelopeSenderUserId)
        store.recordGroupReceipt(
            groupId = groupId,
            authorUserId = identity.userId,
            memberUserId = envelopeSenderUserId,
            receiptType = receipt.receiptType,
            throughLamport = receipt.lamport,
            viaTransport = arrival.transport,
        )
        if (receipt.receiptType == RECEIPT_TYPE_DELIVERED) {
            runCatching {
                store.recordDeliveredMetric(
                    chatId = groupId,
                    throughLamport = receipt.lamport,
                    deliveredAtMs = arrival.receivedAt,
                    viaTransport = arrival.transport,
                )
            }
        }
        ChatEvents.notifyChatChanged(groupId)
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
        val group = store.getGroup(peerUserId)
        if (group != null) {
            emitGroupReadReceipts(group, identity)
            return
        }
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
        val throughLamport = PeerStreamWatermark.through(store, peerUserId, peerUserId)
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

    /**
     * Builds a [uniffi.cruisemesh_core.ReceiptContent] and sends it as a sealed envelope on the exact link [address] (a reply to a frame that just arrived on it).
     *
     * Returns the sealed bytes queued at [address], or 0 if nothing went.
     */
    private fun sendReceiptOnAddress(
        identity: Identity,
        contact: Contact,
        address: String,
        receiptType: UByte,
        ackedSenderUserId: ByteArray,
        throughLamport: ULong,
    ): Long {
        val authored = store.ensureAuthoredReceipt(
            identity,
            contact,
            ackedSenderUserId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        if (!MeshRouter.sendToAddress(address, authored.frame)) return 0L
        return authored.envelope.sealed.size.toLong()
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

    /**
     * Author and send the current delivered/read group watermarks we owe
     * [authorUserId] in [group]. No-op when the author is not a contact
     * (we cannot pairwise-seal to them).
     */
    private fun emitGroupReceiptsToAuthor(
        group: Group,
        authorUserId: ByteArray,
        identity: Identity,
    ) {
        val contact = store.getContact(authorUserId) ?: return
        var queued = false
        for (owed in ReceiptRepair.owedForGroup(store, group.id, authorUserId)) {
            if (queueOutgoingGroupReceiptForRelay(identity, contact, group.id, owed.receiptType, owed.throughLamport)) {
                queued = true
            }
            sendGroupReceiptToContact(identity, contact, group.id, owed.receiptType, owed.throughLamport)
        }
        if (queued) RelaySyncEvents.requestSync()
    }

    private fun emitGroupReadReceipts(group: Group, identity: Identity) {
        if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) return
        for (memberId in group.memberUserIds) {
            if (memberId.contentEquals(identity.userId)) continue
            val throughLamport = PeerStreamWatermark.through(store, group.id, memberId)
            if (throughLamport == 0uL) continue
            store.recordOutgoingReceipt(group.id, memberId, RECEIPT_TYPE_READ, throughLamport)
            emitGroupReceiptsToAuthor(group, memberId, identity)
        }
    }

    /** Receipts we owe [peerUserId] as an author in every shared group. */
    fun syncGroupReceiptsToPeer(
        identity: Identity,
        contact: Contact,
        address: String,
    ): Long {
        var queuedBytes = 0L
        for (group in store.listGroups()) {
            if (!group.memberUserIds.any { it.contentEquals(contact.userId) }) continue
            if (!group.memberUserIds.any { it.contentEquals(identity.userId) }) continue
            for (owed in ReceiptRepair.owedForGroup(store, group.id, contact.userId)) {
                queuedBytes += sendGroupReceiptOnAddress(
                    identity,
                    contact,
                    group.id,
                    address,
                    owed.receiptType,
                    owed.throughLamport,
                )
            }
        }
        return queuedBytes
    }

    private fun queueOutgoingGroupReceiptForRelay(
        identity: Identity,
        author: Contact,
        groupId: ByteArray,
        receiptType: UByte,
        throughLamport: ULong,
        timestamp: Long = System.currentTimeMillis(),
    ): Boolean {
        if (throughLamport == 0uL) return false
        val existing = store.outgoingReceiptEnvelope(groupId, author.userId, receiptType)
        val authored = store.ensureAuthoredGroupReceipt(
            identity,
            author,
            groupId,
            receiptType,
            throughLamport,
            timestamp,
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        return existing == null || existing.throughLamport < authored.envelope.throughLamport
    }

    private fun sendGroupReceiptOnAddress(
        identity: Identity,
        author: Contact,
        groupId: ByteArray,
        address: String,
        receiptType: UByte,
        throughLamport: ULong,
    ): Long {
        val authored = store.ensureAuthoredGroupReceipt(
            identity,
            author,
            groupId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        if (!MeshRouter.sendToAddress(address, authored.frame)) return 0L
        return authored.envelope.sealed.size.toLong()
    }

    private fun sendGroupReceiptToContact(
        identity: Identity,
        author: Contact,
        groupId: ByteArray,
        receiptType: UByte,
        throughLamport: ULong,
    ) {
        val authored = store.ensureAuthoredGroupReceipt(
            identity,
            author,
            groupId,
            receiptType,
            throughLamport,
            System.currentTimeMillis(),
        )
        GossipState.seenIds.record(authored.envelope.msgId)
        if (!MeshRouter.sendToUserId(author.userId, authored.frame)) {
            Log.i(TAG, "Group receipt to ${UserIdHex.encode(author.userId)} queued; not currently connected")
        }
    }

    /**
     * Executes Rust's complete digest-time mule plan, inside the budgets
     * `gate` was issued with.
     *
     * `gate` is core's answer to "may this peer be sprayed, and how much"
     * ([SprayPolicy]); the caller has already checked `gate.allow`. Two
     * further core decisions happen here, both after the plan is built because
     * both need to know what the plan came out as: whether the advertised set
     * is byte-identical to the one this peer was already offered, and what the
     * plan costs this link's burst allowance. A suppressed plan sends nothing
     * and, just as importantly, advances no cursor and records no hidden-kind
     * offer -- everything it would have offered stays exactly as
     * re-discoverable as it was.
     */
    fun sprayDigestPlanTo(
        address: String,
        peerUserId: ByteArray,
        peerKnownMsgIds: List<ByteArray>,
        identity: Identity,
        gate: CoreSprayGate,
        peerAuthenticated: Boolean,
    ) {
        val now = System.currentTimeMillis()
        var carriedReservation: CoreCarriedOfferReservation? = null
        try {
            // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): confirm delivery
            // of anything this digest's advertised `msg_id`s prove the peer
            // already has BEFORE building the spray plan below, so a
            // just-confirmed carried envelope isn't immediately re-sprayed
            // back at the peer who just told us they have it.
            //
            // CARRY-02: durable removal of a carried row is only permitted when
            // the peer identity is authenticated. [peerAuthenticated] is passed
            // in, not re-derived from [address], because it must reflect the
            // transport the digest ARRIVED on -- a LAN link registered only
            // after a completed Noise handshake whose static key matched an
            // accepted contact ([MeshService.onLanPeerAuthenticated]) -- and NOT
            // the link this response is answered on. On the gated-then-replayed
            // path the elected route may have moved to LAN since a BLE digest
            // arrived; re-deriving here would launder that BLE digest's unsigned,
            // spoofable userId and advertised msg_ids into an authenticated
            // removal. For an unauthenticated peer this call deletes nothing and
            // only leaves the spray plan to skip the ids the peer named for this
            // one encounter.
            val confirmed = store.coreConfirmCarriedDeliveries(
                peerUserId,
                peerKnownMsgIds,
                peerAuthenticated,
                now,
            )
            if (confirmed > 0uL) {
                Log.i(TAG, "Confirmed delivery of $confirmed carried envelope(s) to ${UserIdHex.encode(peerUserId)}; dropped our copy")
                // Hard evidence that sprays to this peer are landing: it just
                // proved it holds copies we were carrying. That is what the
                // receipt-quiet backoff is looking for. Its absence is NOT
                // evidence of a fault -- a courier for someone who is not here
                // legitimately confirms nothing.
                SprayPolicy.noteReceiptProgress(peerUserId)
            }
            // How far this link session's walk through our carry queue has
            // got. A courier's store can be many times one round's budget, so
            // each re-digest offers the NEXT page instead of re-reading the
            // oldest rows; once the walk reaches the tail the lane parks until
            // its cooldown elapses. A zero budget is the lane's own off switch.
            val lane = MeshRouter.carriedLaneFor(address, now)
            // G3: cap concurrent foreign-carry offers across peers in a short
            // epoch so a family desk cannot spray the whole store to N peers
            // at once. Reservation is atomic across Android's concurrent BLE,
            // LAN, and relay receive paths. Own mail/receipts still flow when
            // carried is deferred.
            carriedReservation = if (lane.skip) {
                null
            } else {
                carriedOfferGate.tryReserve(now, UserIdHex.encode(peerUserId))
            }
            val allowCarried = carriedReservation != null
            val plan = store.coreDigestSprayPlan(
                ownUserId = identity.userId,
                peerUserId = peerUserId,
                peerHints = recentHintsFor(peerUserId, now),
                peerKnownMsgIds = peerKnownMsgIds,
                nowMs = now,
                carriedBudgetBytes = if (allowCarried) gate.carriedBudgetBytes else 0uL,
                ownOutboundBudgetBytes = gate.ownOutboundBudgetBytes,
                ownReceiptBudgetBytes = gate.ownReceiptBudgetBytes,
                receiptQueryLimit = RELAY_STORE_BATCH_LIMIT,
                peerAcksHiddenKinds = MeshRouter.peerAcksHiddenKinds(address),
                hiddenAlreadyOffered = MeshRouter.hiddenOfferedFor(address),
                carriedCursor = lane.after,
            )
            // Identical-set suppression (#280), asked per lane: 28 consecutive
            // sprays whose authored lane was invariant at 16 envelopes while
            // the carried lane walked its cursor is what the field recorded,
            // and one digest over all three would change on every page turn.
            // Asked here rather than before the plan because the answer is the
            // plan.
            val admission = SprayPolicy.admitPlan(peerUserId, address, plan.lanes)
            val sendCarried = allowCarried && admission.sendCarried
            if (carriedReservation != null) {
                if (!sendCarried || plan.carriedFrames.isEmpty()) {
                    carriedOfferGate.release(carriedReservation)
                } else {
                    carriedOfferGate.commit(carriedReservation)
                }
                carriedReservation = null
            }
            if (!admission.send) {
                Log.i(
                    TAG,
                    "Suppressed an unchanged digest spray to $address " +
                        "(${plan.planBytes} bytes, re-offerable in ${admission.reofferInMs}ms)",
                )
                return
            }
            // Own lanes first, foreign carry last. On a slow link every frame
            // here lands in one FIFO, so whatever goes first delays everything
            // after it: live mail and receipts to real contacts must beat
            // third-party courier traffic. Nothing is lost by deferring the
            // carried lane -- the periodic re-digest offers the next page, and
            // its own per-encounter budget already bounds this round's share.
            val frames =
                (if (admission.sendOwnOutbound) plan.ownOutboundFrames else emptyList()) +
                    (if (admission.sendOwnReceipts) plan.ownReceiptFrames else emptyList()) +
                    (if (sendCarried) plan.carriedFrames else emptyList())
            val sprayed = frames.count { MeshRouter.sendToAddress(address, it) }
            // A refused lane must leave its bookkeeping alone: nothing it would
            // have offered may look offered.
            if (admission.sendOwnOutbound) {
                MeshRouter.recordHiddenOffered(address, plan.offeredHiddenMsgIds)
            }
            if (sendCarried) {
                MeshRouter.recordCarriedProgress(address, plan.nextCarriedCursor, plan.carriedExhausted, now)
            }
            val carriedNote = when {
                !allowCarried -> ", carried deferred (cap/park)"
                !admission.sendCarried -> ", carried unchanged"
                else -> ""
            }
            val authoredNote = if (!admission.sendOwnOutbound) ", authored unchanged" else ""
            Log.i(
                TAG,
                "Digest spray to $address sent $sprayed/${frames.size} frame(s) " +
                    "(carried=${plan.carriedFrames.size}, authored=${plan.ownOutboundFrames.size}, " +
                    "receipts=${plan.ownReceiptFrames.size}$carriedNote$authoredNote)",
            )
        } catch (e: CoreException) {
            Log.w(TAG, "Failed to build digest spray plan for $address: ${e.message}")
        } finally {
            carriedReservation?.let(carriedOfferGate::release)
        }
    }
}
