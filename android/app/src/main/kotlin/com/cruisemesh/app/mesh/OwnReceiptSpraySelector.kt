package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope

/**
 * Pure selection logic backing [MeshService]'s digest-time receipt mule spray
 * (BLE_1TO1_MULING.md §6 follow-up): of our own still-undelivered outgoing
 * receipt envelopes (DELIVERED/READ acks we owe the *original message
 * senders*), which ones are worth handing to the peer we're digest-syncing
 * with so it can mule them back toward those senders. This is the receipt
 * analogue of [OwnOutboundSpraySelector]; without it a pure-offline
 * A -> mule -> C hop delivers the message but strands C's receipt, so A's
 * tick stays "sent" forever even though C received and read it.
 *
 * Split out as a pure function (no Android/store deps) so the exclusion and
 * budget rules are unit-testable, following the pattern set by
 * [OwnOutboundSpraySelector] and [MeshRouterState].
 */
object OwnReceiptSpraySelector {
    /**
     * [pending] is the store's relay-uploadable receipt queue
     * ([uniffi.cruisemesh_core.MessageStore.pendingRelayOutgoingReceiptEnvelopes]),
     * already filtered by the core to un-posted, unexpired rows ordered
     * oldest-first (queued_at ascending).
     *
     * A receipt whose [OutgoingReceiptEnvelope.recipientUserId] equals
     * [peerUserId] is skipped: that receipt is owed to the very peer we're
     * syncing with, and [MeshService.syncReceiptsFirst] already hands it to
     * them directly on this same digest exchange -- muling it through them to
     * themselves would be pointless. This mirrors [OwnOutboundSpraySelector]
     * excluding the peer's own chat.
     *
     * [peerKnownMsgIdsHex] is the digest peer's already-carried `msg_id` set
     * (the same field [MeshService.sprayCarriedEnvelopesTo] suppresses on),
     * hex-encoded via [UserIdHex.encode] because [ByteArray] has no structural
     * equality -- a receipt the mule already carries isn't re-sent the same
     * bytes on every reconnect.
     *
     * Envelopes are taken oldest-first, consuming [budgetBytes] (measured as
     * [OutgoingReceiptEnvelope.sealed] size); the walk stops the moment the
     * budget would be exceeded so one digest exchange makes bounded progress.
     * Receipts are tiny, so in practice the budget rarely bites -- it is a
     * backstop against a pathological backlog, not a normal-case limiter.
     */
    fun select(
        pending: List<OutgoingReceiptEnvelope>,
        peerUserId: ByteArray,
        peerKnownMsgIdsHex: Set<String>,
        nowMs: Long,
        budgetBytes: Long,
    ): List<OutgoingReceiptEnvelope> {
        val selected = mutableListOf<OutgoingReceiptEnvelope>()
        var used = 0L
        for (envelope in pending) {
            if (envelope.recipientUserId.contentEquals(peerUserId)) continue
            if (envelope.expiry <= nowMs) continue
            if (UserIdHex.encode(envelope.msgId) in peerKnownMsgIdsHex) continue
            val size = envelope.sealed.size.toLong()
            if (used + size > budgetBytes) break
            selected += envelope
            used += size
        }
        return selected
    }
}
