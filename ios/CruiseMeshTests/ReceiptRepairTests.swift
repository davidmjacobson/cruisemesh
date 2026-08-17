import XCTest
@testable import CruiseMesh

/// The receipt-repair lane, pinned against the self-lock it used to have.
///
/// A DELIVERED receipt to a directly connected peer has exactly three lanes,
/// and two of them cannot heal a lost receipt by design: the delivery-time
/// receipt fires only for a newly inserted message, and the digest receipt
/// spray is mule-only (it excludes receipts addressed to the connected peer).
/// That leaves `ReceiptRepair` as the only repair path -- and it used to cap
/// each watermark at the peer's digest entry for its own authored stream, and
/// hard-return when that entry was 0. The digest entry is the *contiguous*
/// lamport, which a front gap pins to 0 forever, so the repair receipt was
/// pinned to 0 forever too and the sender replayed its backlog on every send.
final class ReceiptRepairTests: XCTestCase {
    private func userId(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    private func insertPeerMessage(
        store: MessageStore,
        chatId: Data,
        peer: Data,
        lamport: UInt64,
        kind: UInt8 = ProtocolKind.text
    ) throws {
        _ = try store.insertMessage(message: StoredMessage(
            chatId: chatId,
            senderUserId: peer,
            lamport: lamport,
            timestamp: Int64(lamport) * 1_000,
            kind: kind,
            payload: Data(),
            senderDeviceId: coreLegacyDeviceId()
        ))
    }

    /// The bug, end to end. The peer's own authored stream has a permanent
    /// front gap (a chat wipe or a backup restore ratchets their next authored
    /// lamport above anything either side still holds), so their digest reports
    /// 0 for themselves forever -- while our receipt watermark, correctly MAX,
    /// sits at 4. The repair lane must send the full 4.
    func testFrontGapInThePeerStreamStillGetsAFullWatermarkRepairReceipt() throws {
        let store = try MessageStore.open(path: ":memory:")
        let peer = userId(2)
        try insertPeerMessage(store: store, chatId: peer, peer: peer, lamport: 3)
        try insertPeerMessage(store: store, chatId: peer, peer: peer, lamport: 4)

        let through = PeerStreamWatermark.through(store: store, chatId: peer, senderUserId: peer)
        XCTAssertEqual(through, 4)
        try store.recordOutgoingReceipt(
            chatId: peer, senderUserId: peer,
            receiptType: ReceiptType.delivered, throughLamport: through
        )
        try store.recordOutgoingReceipt(
            chatId: peer, senderUserId: peer,
            receiptType: ReceiptType.read, throughLamport: through
        )

        // This is the number the old cap consulted: the peer's digest entry
        // for its own stream, which is the contiguous count and stops dead
        // before lamport 3. Nothing may be capped or gated against it.
        XCTAssertEqual(try store.highestContiguousLamport(chatId: peer, senderUserId: peer), 0)

        XCTAssertEqual(
            ReceiptRepair.owedTo(store: store, peerUserId: peer),
            [
                OwedReceipt(receiptType: ReceiptType.delivered, throughLamport: 4),
                OwedReceipt(receiptType: ReceiptType.read, throughLamport: 4)
            ]
        )
    }

    /// A peer we owe nothing gets nothing -- the lane stays quiet, it just
    /// never caps.
    func testPeerWithNoRecordedWatermarkIsOwedNoRepairReceipts() throws {
        let store = try MessageStore.open(path: ":memory:")
        XCTAssertEqual(ReceiptRepair.owedTo(store: store, peerUserId: userId(7)), [])
    }

    /// DELIVERED can be owed on its own; a chat never opened owes no READ.
    func testOnlyTheWatermarksActuallyRecordedAreRepaired() throws {
        let store = try MessageStore.open(path: ":memory:")
        let peer = userId(3)
        try insertPeerMessage(store: store, chatId: peer, peer: peer, lamport: 9)
        try store.recordOutgoingReceipt(
            chatId: peer, senderUserId: peer,
            receiptType: ReceiptType.delivered, throughLamport: 9
        )

        XCTAssertEqual(
            ReceiptRepair.owedTo(store: store, peerUserId: peer),
            [OwedReceipt(receiptType: ReceiptType.delivered, throughLamport: 9)]
        )
    }

    /// A group invite is authored into the 1:1 pairwise stream but filed under
    /// the group's chat id, so the 1:1 chat gains no row at its lamport. At the
    /// tail of the stream that stranded the delivered watermark below it
    /// forever. The invite's ack now raises the floor to its own lamport.
    func testGroupInviteAtTheStreamTailNoLongerStrandsTheDeliveredWatermark() throws {
        let store = try MessageStore.open(path: ":memory:")
        let peer = userId(4)
        try insertPeerMessage(store: store, chatId: peer, peer: peer, lamport: 1)
        try insertPeerMessage(store: store, chatId: peer, peer: peer, lamport: 2)
        // The invite arrives at lamport 3 and is filed under the group chat.
        try insertPeerMessage(
            store: store, chatId: userId(8), peer: peer,
            lamport: 3, kind: ProtocolKind.groupInvite
        )

        // Without the floor the 1:1 watermark stops at 2 -- below the invite.
        XCTAssertEqual(PeerStreamWatermark.through(store: store, chatId: peer, senderUserId: peer), 2)
        XCTAssertEqual(
            PeerStreamWatermark.through(
                store: store, chatId: peer, senderUserId: peer, atLeastLamport: 3
            ),
            3
        )
    }
}
