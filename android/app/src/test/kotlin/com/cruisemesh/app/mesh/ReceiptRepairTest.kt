package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.coreLegacyDeviceId

/**
 * The receipt-repair lane, pinned against the self-lock it used to have.
 *
 * A DELIVERED receipt to a directly connected peer has exactly three lanes,
 * and two of them cannot heal a lost receipt by design: the delivery-time
 * receipt fires only for a newly inserted message, and the digest receipt
 * spray is mule-only (it excludes receipts addressed to the connected peer).
 * That leaves [ReceiptRepair] as the only repair path -- and it used to cap
 * each watermark at the peer's digest entry for its own authored stream, and
 * hard-return when that entry was 0. The digest entry is the *contiguous*
 * lamport, which a front gap pins to 0 forever, so the repair receipt was
 * pinned to 0 forever too and the sender replayed its backlog on every send.
 */
class ReceiptRepairTest {

    companion object {
        init {
            HostCoreLibrary.load()
        }
    }

    private fun userId(byte: Byte): ByteArray = ByteArray(16) { byte }

    private fun insertPeerMessage(store: MessageStore, peer: ByteArray, lamport: ULong, kind: UByte = 1u) {
        store.insertMessage(
            StoredMessage(
                chatId = peer,
                senderUserId = peer,
                lamport = lamport,
                timestamp = lamport.toLong() * 1_000L,
                kind = kind,
                payload = ByteArray(0),
                senderDeviceId = coreLegacyDeviceId(),
            ),
        )
    }

    /**
     * The bug, end to end. The peer's own authored stream has a permanent
     * front gap (a chat wipe or a backup restore ratchets their next authored
     * lamport above anything either side still holds), so their digest reports
     * 0 for themselves forever -- while our receipt watermark, correctly MAX,
     * sits at 4. The repair lane must send the full 4.
     */
    @Test
    fun `a front gap in the peer stream still gets a full watermark repair receipt`() {
        val store = MessageStore.open(":memory:")
        val peer = userId(2)
        insertPeerMessage(store, peer, 3uL)
        insertPeerMessage(store, peer, 4uL)

        val through = PeerStreamWatermark.through(store, peer, peer)
        assertEquals(4uL, through)
        store.recordOutgoingReceipt(peer, peer, RECEIPT_TYPE_DELIVERED, through)
        store.recordOutgoingReceipt(peer, peer, RECEIPT_TYPE_READ, through)

        // This is the number the old cap consulted: the peer's digest entry
        // for its own stream, which is the contiguous count and stops dead
        // before lamport 3. Nothing may be capped or gated against it.
        assertEquals(0uL, store.highestContiguousLamport(peer, peer))

        assertEquals(
            listOf(
                OwedReceipt(RECEIPT_TYPE_DELIVERED, 4uL),
                OwedReceipt(RECEIPT_TYPE_READ, 4uL),
            ),
            ReceiptRepair.owedTo(store, peer),
        )
    }

    /** A peer we owe nothing gets nothing -- the lane stays quiet, it just never caps. */
    @Test
    fun `a peer with no recorded watermark is owed no repair receipts`() {
        val store = MessageStore.open(":memory:")
        assertEquals(emptyList<OwedReceipt>(), ReceiptRepair.owedTo(store, userId(7)))
    }

    /** DELIVERED can be owed on its own; a chat never opened owes no READ. */
    @Test
    fun `only the watermarks actually recorded are repaired`() {
        val store = MessageStore.open(":memory:")
        val peer = userId(3)
        insertPeerMessage(store, peer, 9uL)
        store.recordOutgoingReceipt(peer, peer, RECEIPT_TYPE_DELIVERED, 9uL)

        assertEquals(
            listOf(OwedReceipt(RECEIPT_TYPE_DELIVERED, 9uL)),
            ReceiptRepair.owedTo(store, peer),
        )
    }

    /**
     * A group invite is authored into the 1:1 pairwise stream but filed under
     * the group's chat id, so the 1:1 chat gains no row at its lamport. At the
     * tail of the stream that stranded the delivered watermark below it
     * forever. The invite's ack now raises the floor to its own lamport.
     */
    @Test
    fun `a group invite at the stream tail no longer strands the delivered watermark`() {
        val store = MessageStore.open(":memory:")
        val peer = userId(4)
        insertPeerMessage(store, peer, 1uL)
        insertPeerMessage(store, peer, 2uL)
        // The invite arrives at lamport 3 and is filed under the group chat.
        store.insertMessage(
            StoredMessage(
                chatId = userId(8),
                senderUserId = peer,
                lamport = 3uL,
                timestamp = 3_000L,
                kind = KIND_GROUP_INVITE,
                payload = ByteArray(0),
                senderDeviceId = coreLegacyDeviceId(),
            ),
        )

        // Without the floor the 1:1 watermark stops at 2 -- below the invite.
        assertEquals(2uL, PeerStreamWatermark.through(store, peer, peer))
        assertEquals(3uL, PeerStreamWatermark.through(store, peer, peer, atLeastLamport = 3uL))
    }
}
