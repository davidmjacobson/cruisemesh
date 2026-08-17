import XCTest
@testable import CruiseMesh

/// Regression cover for the receipt watermark stalling on a gapped peer
/// stream -- the "stuck on one checkmark even though it arrived" bug. Android
/// moved its peer-stream receipts off `highestContiguousLamport` in 6fd20c2
/// (consolidated into `acknowledgePeerStream` by #119); these pin the same
/// semantics for the iOS shell.
final class PeerStreamWatermarkTests: XCTestCase {
    private func userId(_ byte: UInt8) -> Data { Data(repeating: byte, count: 16) }

    private func insertPeerMessage(
        store: MessageStore,
        peer: Data,
        lamport: UInt64,
        kind: UInt8 = ProtocolKind.text
    ) throws {
        _ = try store.insertMessage(message: StoredMessage(
            chatId: peer,
            senderUserId: peer,
            lamport: lamport,
            timestamp: Int64(lamport) * 1_000,
            kind: kind,
            payload: Data(),
            senderDeviceId: coreLegacyDeviceId()
        ))
    }

    /// The kind-9-shaped case: this build does not handle one of the kinds the
    /// peer sent, so no `messages` row is ever written for lamport 3 and the
    /// hole is permanent. The watermark must still advance to 4, otherwise
    /// every later message is under-reported as undelivered forever.
    func testWatermarkAdvancesPastAGapLeftByAnUnhandledKind() throws {
        let store = try MessageStore.open(path: ":memory:")
        let peer = userId(2)
        try insertPeerMessage(store: store, peer: peer, lamport: 1)
        try insertPeerMessage(store: store, peer: peer, lamport: 2)
        // lamport 3 is the kind this build drops -- no row, and none is coming.
        try insertPeerMessage(store: store, peer: peer, lamport: 4)

        XCTAssertEqual(
            PeerStreamWatermark.through(store: store, chatId: peer, senderUserId: peer),
            4
        )

        // The bug this replaces: the contiguous count stops dead at the hole,
        // so lamport 4 could never be acknowledged no matter how many more
        // messages arrived.
        XCTAssertEqual(
            try store.highestContiguousLamport(chatId: peer, senderUserId: peer),
            2
        )
    }

    /// The front-gap case Android hit first: the lamport ratchet rebases a
    /// peer's stream above 1 after a chat-history wipe, so lamports 1 and 2
    /// never existed for anyone and the contiguous count reports 0 forever.
    func testWatermarkAdvancesOverAFrontGapFromTheLamportRatchet() throws {
        let store = try MessageStore.open(path: ":memory:")
        let peer = userId(3)
        try insertPeerMessage(store: store, peer: peer, lamport: 3)
        try insertPeerMessage(store: store, peer: peer, lamport: 4)

        XCTAssertEqual(
            PeerStreamWatermark.through(store: store, chatId: peer, senderUserId: peer),
            4
        )
        XCTAssertEqual(
            try store.highestContiguousLamport(chatId: peer, senderUserId: peer),
            0
        )
    }

    /// Widening the *receipt* watermark must not widen gap detection. The
    /// digest still reports the contiguous prefix, so digest sync keeps
    /// re-requesting genuinely lost messages -- and the DTN carry path, which
    /// drops a carried envelope only on digest proof of receipt, keeps seeing
    /// the hole and holds its copy.
    func testDigestStillStopsAtTheGapSoCarriedCopiesAreNotDroppedEarly() throws {
        let store = try MessageStore.open(path: ":memory:")
        let peer = userId(4)
        try insertPeerMessage(store: store, peer: peer, lamport: 1)
        try insertPeerMessage(store: store, peer: peer, lamport: 2)
        try insertPeerMessage(store: store, peer: peer, lamport: 4)

        let entries = try store.chatDigest(chatId: peer)
        let entry = try XCTUnwrap(entries.first { $0.senderUserId == peer })
        XCTAssertEqual(entry.throughLamport, 2)
    }

    /// A stream we hold nothing from acknowledges nothing; callers gate on
    /// `> 0` and must keep seeing 0 here.
    func testWatermarkIsZeroForAnEmptyStream() throws {
        let store = try MessageStore.open(path: ":memory:")
        XCTAssertEqual(
            PeerStreamWatermark.through(store: store, chatId: userId(5), senderUserId: userId(5)),
            0
        )
    }

    /// The watermark is per sender, not per chat: one member's gap must not
    /// bleed into another member's receipt in a group.
    func testWatermarkIsPerSenderNotPerChat() throws {
        let store = try MessageStore.open(path: ":memory:")
        let groupId = userId(9)
        let alice = userId(1)
        let bob = userId(2)
        _ = try store.insertMessage(message: StoredMessage(
            chatId: groupId, senderUserId: alice, lamport: 7,
            timestamp: 7_000, kind: ProtocolKind.text, payload: Data(),
            senderDeviceId: coreLegacyDeviceId()
        ))
        _ = try store.insertMessage(message: StoredMessage(
            chatId: groupId, senderUserId: bob, lamport: 2,
            timestamp: 2_000, kind: ProtocolKind.text, payload: Data(),
            senderDeviceId: coreLegacyDeviceId()
        ))

        XCTAssertEqual(
            PeerStreamWatermark.through(store: store, chatId: groupId, senderUserId: alice),
            7
        )
        XCTAssertEqual(
            PeerStreamWatermark.through(store: store, chatId: groupId, senderUserId: bob),
            2
        )
    }
}
