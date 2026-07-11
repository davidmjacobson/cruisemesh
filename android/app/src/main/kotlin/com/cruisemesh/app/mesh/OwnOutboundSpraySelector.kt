package com.cruisemesh.app.mesh

import com.cruisemesh.app.chat.UserIdHex
import uniffi.cruisemesh_core.OutboundEnvelope

/**
 * Pure selection logic backing [MeshService]'s digest-time mule spray
 * (BLE_1TO1_MULING.md Hook B): of our own still-undelivered 1:1 outbound
 * envelopes to contacts *other than* the peer we're digest-syncing with,
 * which ones are worth handing to that peer right now so it can mule them
 * onward. Split out as a pure function (no Android/store deps) so the
 * ordering and budget rules are unit-testable, following the pattern set by
 * [MeshRouterState] and [com.cruisemesh.app.chat.nextAuthoredLamport].
 */
object OwnOutboundSpraySelector {
    /**
     * [pendingByRecipient] holds, per non-peer contact, that contact's
     * outbound envelopes already filtered by the caller to "not yet acked
     * DELIVERED" and ordered oldest-first (lamport ascending, matching
     * [uniffi.cruisemesh_core.MessageStore.outboundEnvelopesAfter]'s own
     * order). Recipients are walked in the given list order; within each
     * recipient, envelopes are taken oldest-first, consuming [budgetBytes]
     * (measured as [OutboundEnvelope.sealed] size) across every recipient
     * combined. The walk stops the moment the budget would be exceeded --
     * it does not skip ahead to a later recipient's smaller mail -- so one
     * digest exchange makes bounded, predictable progress rather than an
     * unbounded scan over every contact every time.
     *
     * [peerKnownMsgIdsHex] is the digest peer's already-known carried
     * `msg_id` set (see [MeshService.sprayCarriedEnvelopesTo]'s sibling use
     * of the same digest field), hex-encoded via [UserIdHex.encode] because
     * [ByteArray] has no structural equality -- an envelope whose `msgId`
     * is in this set is skipped so a mule that already carries it isn't
     * re-sent the same bytes on every reconnect.
     */
    fun select(
        pendingByRecipient: List<List<OutboundEnvelope>>,
        peerKnownMsgIdsHex: Set<String>,
        nowMs: Long,
        budgetBytes: Long,
    ): List<OutboundEnvelope> {
        val selected = mutableListOf<OutboundEnvelope>()
        var used = 0L
        outer@ for (pending in pendingByRecipient) {
            for (envelope in pending) {
                if (envelope.expiry <= nowMs) continue
                if (UserIdHex.encode(envelope.msgId) in peerKnownMsgIdsHex) continue
                val size = envelope.sealed.size.toLong()
                if (used + size > budgetBytes) break@outer
                selected += envelope
                used += size
            }
        }
        return selected
    }
}
