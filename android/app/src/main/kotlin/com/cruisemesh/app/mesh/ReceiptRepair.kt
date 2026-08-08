package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.MessageStore

/** One receipt the repair lane owes a peer: a type and the watermark it covers. */
data class OwedReceipt(val receiptType: UByte, val throughLamport: ULong)

/**
 * The receipt-repair lane: on peer sync, re-send the cumulative
 * delivered/read watermarks we owe this peer, so a receipt that was lost (or
 * couldn't be sent when it was first observed) heals on reconnect.
 *
 * This is the *only* lane that can heal a lost receipt to a directly
 * connected peer, which is why it is deliberately unconditional:
 *
 * - The delivery-time receipt fires only for a newly INSERTED message (both
 *   shells guard on `inserted`), so a backlog replay never re-acks.
 * - The digest receipt spray (`select_own_receipts`) excludes receipts
 *   addressed to the connected peer -- that lane is mule-only.
 *
 * If this lane also declines to send, the pairing self-locks: the sender never
 * learns anything landed and replays its whole backlog on every send.
 *
 * See [owedTo] for why nothing here consults the peer's digest.
 */
object ReceiptRepair {

    /**
     * The receipts we owe [peerUserId] right now, zero watermarks dropped.
     *
     * Deliberately takes no digest. This used to cap each watermark at the
     * peer's `chatDigest` entry for its own authored stream (and hard-return
     * when that entry was 0), on the reasoning that acking beyond what the
     * peer says it authored is nonsensical. The comparison was invalid: the
     * two numbers are different measures. The digest entry is
     * `highestContiguousLamport`, which stops dead at the first gap, while a
     * receipt watermark is deliberately a plain MAX (see
     * [PeerStreamWatermark]). A front gap in the peer's own authored stream --
     * routine after a chat wipe or a backup restore, both of which ratchet the
     * next authored lamport above anything either side still holds -- pins the
     * digest entry at 0 permanently. The cap then pinned every repair receipt
     * to 0 permanently too, which is exactly the self-lock above.
     *
     * Uncapped is safe. The receiving side's `record_receipt` is validated and
     * strictly monotonic (`MAX(stored, incoming)`). Since #283 it does retire
     * the `outbound_envelopes` rows a delivered watermark covers, so an
     * over-reported watermark removes sealed rows early -- but it removes only
     * the sealed retransmission artifact, and only where the `messages` row
     * that regenerates it survives, so the sender still holds the message and
     * [respondToDigest]'s backfill re-seals it if that peer later reports the
     * gap in its own gap-aware digest. An over-report therefore costs at worst
     * a slightly early delivered tick, never a message the sender still owes.
     * A capped watermark is fatal: it stalls forever.
     */
    fun owedTo(store: MessageStore, peerUserId: ByteArray): List<OwedReceipt> =
        listOf(RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ)
            .map { type -> OwedReceipt(type, store.outgoingReceiptThrough(peerUserId, peerUserId, type)) }
            .filter { it.throughLamport > 0uL }
}
