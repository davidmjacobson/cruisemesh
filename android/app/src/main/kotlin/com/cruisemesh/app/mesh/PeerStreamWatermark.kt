package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.MessageStore

/**
 * The single place that answers "how far through this peer's stream have we
 * actually got?" for a delivered/read receipt. Mirrors iOS's
 * `PeerStreamWatermark.swift`, which this consolidates the Android side onto.
 *
 * `highestLamport` (plain MAX), **not** `highestContiguousLamport`: this is a
 * watermark over a *peer's* stream, and that stream can legitimately contain
 * lamports we will never hold a row for. `highestContiguousLamport` stops at
 * the first missing lamport and then reports the same number forever, so
 * basing a receipt on it stalls the sender's checkmarks permanently. Three
 * ways a peer stream legitimately gains a hole:
 *
 * - **A front gap from the lamport ratchet.** After a chat-history wipe (or a
 *   backup restore) a sender's stream restarts above lamport 1; lamports below
 *   the new base never existed for anyone, so `highestContiguousLamport`
 *   reports 0 and never moves.
 * - **An interior gap from a message kind this build does not handle.** An
 *   older build that receives a newer sideband kind drops it without writing a
 *   `messages` row, so that lamport is missing here forever even though the
 *   peer sent it and every later message arrives fine.
 * - **A lamport whose row we filed somewhere else.** A group invite is
 *   authored into the 1:1 pairwise stream but stored under the group's chat
 *   id, so the 1:1 chat never gains a row at that lamport -- see
 *   [atLeastLamport].
 *
 * Only the *receipt* watermark widens. Gap detection still belongs to
 * `chatDigest`, which keeps using `highestContiguousLamport` so digest sync
 * can still spot and re-request genuinely lost messages -- and so the DTN
 * carry path, which removes a carried envelope only on digest proof of
 * receipt, is untouched by this.
 */
object PeerStreamWatermark {

    /**
     * The cumulative lamport a receipt for [senderUserId]'s messages in
     * [chatId] should report. 0 when we hold nothing from them, which every
     * caller treats as "nothing to acknowledge yet".
     *
     * [atLeastLamport] raises the floor for a message we genuinely consumed
     * but did not file under [chatId] -- the group invite case. The invite
     * rides the 1:1 pairwise lamport stream (so the sender's next authored
     * lamport is above it) but its row lives under the group's chat id, so a
     * pure MAX over the 1:1 chat sits below it. Left at 0 the invite would
     * strand the peer's delivered watermark under its own lamport for as long
     * as it stayed at the tail of the stream, and the sender would replay its
     * backlog forever.
     */
    fun through(
        store: MessageStore,
        chatId: ByteArray,
        senderUserId: ByteArray,
        atLeastLamport: ULong = 0uL,
    ): ULong = maxOf(store.highestLamport(chatId, senderUserId), atLeastLamport)
}
