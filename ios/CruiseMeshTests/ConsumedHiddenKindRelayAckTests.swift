import XCTest
@testable import CruiseMesh

/// The relay-mailbox growth rule: what a phone may record about a "hidden"
/// kind it consumed, and which relay rows that record then lets it delete.
///
/// A receipt is the highest-volume kind on the wire (one delivered plus one
/// read watermark per message) and it leaves no `messages` row, so before the
/// consumed-hidden-kind set its relay copy could never be acked: the phone
/// consumed the envelope over Bluetooth first, the relay copy deduped as SEEN
/// a moment later, and the row then sat in the mailbox for its whole 7-day
/// expiry. A real mailbox reached ~29k rows this way.
///
/// `MeshController` itself is not unit-testable here (CoreBluetooth,
/// `@MainActor`), so these drive the exact two core calls it makes at the one
/// point it may vouch for an envelope -- `openMessage` against our own key,
/// then `coreRecordConsumedHiddenMsgId` -- and then ask core for the ack
/// decision. The negative cases matter at least as much as the positive one:
/// nothing muled, nothing under a group's shared hint, and nothing we merely
/// heard about may ever be acked.
///
/// Android twin: `ConsumedHiddenKindRelayAckTest.kt`, case for case.
final class ConsumedHiddenKindRelayAckTests: XCTestCase {

    private func contact(for identity: Identity, name: String) -> Contact {
        Contact(
            userId: identity.userId,
            name: name,
            signPk: identity.signPk,
            agreePk: identity.agreePk,
            relayUrl: nil,
            relayToken: nil,
            nickname: nil
        )
    }

    /// Authors a real DELIVERED receipt from `sender` to `recipient`, as the
    /// sender's own relay-sync pass would: pairwise-sealed, addressed to the
    /// recipient's own daily hint.
    private func receipt(
        from sender: Identity,
        senderStore: MessageStore,
        to recipient: Identity,
        now: Int64
    ) throws -> OutgoingReceiptEnvelope {
        try senderStore.ensureAuthoredReceipt(
            identity: sender,
            contact: contact(for: recipient, name: "Recipient"),
            ackedSenderUserId: recipient.userId,
            receiptType: ReceiptType.delivered,
            throughLamport: 3,
            timestampMs: now
        ).envelope
    }

    /// The two calls `MeshController.processInboundEnvelope` makes on the
    /// pairwise-consumed path, in order: prove the envelope was sealed to us,
    /// then record that we consumed it.
    @discardableResult
    private func consumeAsEndpoint(
        _ envelope: OutgoingReceiptEnvelope,
        as identity: Identity,
        store: MessageStore,
        kind: UInt8 = ProtocolKind.receipt,
        now: Int64
    ) throws -> Bool {
        _ = try openMessage(recipient: identity, sealed: envelope.sealed)
        return try store.coreRecordConsumedHiddenMsgId(
            msgId: envelope.msgId,
            kind: kind,
            recipientHint: envelope.recipientHint,
            expiryMs: envelope.expiry,
            ownUserId: identity.userId,
            nowMs: now
        )
    }

    private func seen(
        _ relayId: Int64,
        _ msgId: Data,
        _ hint: Data
    ) -> CoreRelayEnvelopeDisposition {
        CoreRelayEnvelopeDisposition(
            relayId: relayId,
            msgId: msgId,
            disposition: .seen,
            recipientHint: hint
        )
    }

    // MARK: - the decision table, as a pure function

    func testConsumedSeenWithHiddenNeedsBothTheRecordAndAnOwnHint() {
        let own = Data(repeating: 9, count: 16)
        XCTAssertTrue(coreConsumedSeenIsAckableWithHidden(
            origin: nil, ownUserId: own, recordedConsumedHidden: true, hintIsOwnSelfHint: true
        ))
        XCTAssertFalse(coreConsumedSeenIsAckableWithHidden(
            origin: nil, ownUserId: own, recordedConsumedHidden: true, hintIsOwnSelfHint: false
        ))
        XCTAssertFalse(coreConsumedSeenIsAckableWithHidden(
            origin: nil, ownUserId: own, recordedConsumedHidden: false, hintIsOwnSelfHint: true
        ))
        XCTAssertFalse(coreConsumedSeenIsAckableWithHidden(
            origin: nil, ownUserId: own, recordedConsumedHidden: false, hintIsOwnSelfHint: false
        ))
    }

    func testTheMessagesRowAlwaysWinsOverTheHiddenEvidence() {
        let own = Data(repeating: 9, count: 16)
        let them = Data(repeating: 1, count: 16)
        // A 1:1 row is ackable on its own evidence.
        XCTAssertTrue(coreConsumedSeenIsAckableWithHidden(
            origin: MessageOrigin(chatId: them, senderUserId: them),
            ownUserId: own,
            recordedConsumedHidden: false,
            hintIsOwnSelfHint: false
        ))
        // A group row is not, and the hidden evidence must not override it --
        // the un-acking answer always wins.
        XCTAssertFalse(coreConsumedSeenIsAckableWithHidden(
            origin: MessageOrigin(chatId: Data(repeating: 7, count: 16), senderUserId: them),
            ownUserId: own,
            recordedConsumedHidden: true,
            hintIsOwnSelfHint: true
        ))
        // Nor may our own outbound echo be acked: that copy is the
        // recipient's.
        XCTAssertFalse(coreConsumedSeenIsAckableWithHidden(
            origin: MessageOrigin(chatId: them, senderUserId: own),
            ownUserId: own,
            recordedConsumedHidden: true,
            hintIsOwnSelfHint: true
        ))
    }

    func testOnlyKindsWithoutAMessagesRowAreHidden() {
        for kind in [ProtocolKind.text, ProtocolKind.attachmentManifest,
                     ProtocolKind.reaction, ProtocolKind.groupMetadataUpdate] {
            XCTAssertTrue(coreKindPersistsMsgIdRow(kind: kind), "kind \(kind)")
        }
        for kind in [ProtocolKind.receipt, ProtocolKind.friendRequest, ProtocolKind.groupInvite,
                     ProtocolKind.profileSync, ProtocolKind.friendDirectory,
                     ProtocolKind.introducedFriendRequest, ProtocolKind.lanEndpointHint,
                     ProtocolKind.relayUpdate] {
            XCTAssertFalse(coreKindPersistsMsgIdRow(kind: kind), "kind \(kind)")
        }
    }

    // MARK: - recording, against the real store

    func testReceiptConsumedOverBleIsAckedWhenItsRelayCopyTurnsUpAsSeen() throws {
        let alice = generateIdentity()
        let bob = generateIdentity()
        let aliceStore = try MessageStore.open(path: ":memory:")
        let bobStore = try MessageStore.open(path: ":memory:")
        try aliceStore.upsertContact(contact: contact(for: bob, name: "Bob"))
        try bobStore.upsertContact(contact: contact(for: alice, name: "Alice"))
        let now: Int64 = 1_700_000_000_000

        let envelope = try receipt(from: alice, senderStore: aliceStore, to: bob, now: now)
        XCTAssertTrue(try consumeAsEndpoint(envelope, as: bob, store: bobStore, now: now))
        XCTAssertTrue(try bobStore.consumedHiddenMsgIdRecorded(msgId: envelope.msgId, nowMs: now))

        let acked = try bobStore.coreRelayAckIdsWithConsumed(
            items: [seen(4_242, envelope.msgId, envelope.recipientHint)],
            ownUserId: bob.userId,
            nowMs: now
        )
        XCTAssertEqual(acked, [4_242], "the already-consumed relay copy must be deleted")
    }

    func testAReceiptThisDeviceNeverConsumedIsNeverAcked() throws {
        let alice = generateIdentity()
        let bob = generateIdentity()
        let aliceStore = try MessageStore.open(path: ":memory:")
        let bobStore = try MessageStore.open(path: ":memory:")
        try aliceStore.upsertContact(contact: contact(for: bob, name: "Bob"))
        let now: Int64 = 1_700_000_000_000

        let envelope = try receipt(from: alice, senderStore: aliceStore, to: bob, now: now)
        XCTAssertFalse(try bobStore.consumedHiddenMsgIdRecorded(msgId: envelope.msgId, nowMs: now))

        let acked = try bobStore.coreRelayAckIdsWithConsumed(
            items: [seen(7, envelope.msgId, envelope.recipientHint)],
            ownUserId: bob.userId,
            nowMs: now
        )
        XCTAssertTrue(acked.isEmpty, "nothing may be acked on no evidence")
    }

    func testAnEnvelopeAddressedToSomeoneElseIsNeverRecorded() throws {
        // What a proxy-fetched / muled copy looks like: Bob is not its
        // endpoint, so its relay copy is Carol's durable fallback.
        let alice = generateIdentity()
        let bob = generateIdentity()
        let carol = generateIdentity()
        let aliceStore = try MessageStore.open(path: ":memory:")
        let bobStore = try MessageStore.open(path: ":memory:")
        try aliceStore.upsertContact(contact: contact(for: carol, name: "Carol"))
        let now: Int64 = 1_700_000_000_000

        let envelope = try receipt(from: alice, senderStore: aliceStore, to: carol, now: now)
        // Bob cannot even open it -- but if a future call site ever reached
        // the recorder anyway, the hint check must still refuse.
        XCTAssertThrowsError(try openMessage(recipient: bob, sealed: envelope.sealed))
        XCTAssertFalse(try bobStore.coreRecordConsumedHiddenMsgId(
            msgId: envelope.msgId,
            kind: ProtocolKind.receipt,
            recipientHint: envelope.recipientHint,
            expiryMs: envelope.expiry,
            ownUserId: bob.userId,
            nowMs: now
        ))
        XCTAssertEqual(try bobStore.consumedHiddenMsgIdCount(), 0)
    }

    func testAConsumedHiddenKindIsStillNeverAckedUnderAGroupsSharedHint() throws {
        // The one shape that could otherwise slip through: the msg_id is
        // genuinely recorded, but the relay hands the row back addressed to a
        // group's shared hint, which every member fetches.
        let alice = generateIdentity()
        let bob = generateIdentity()
        let aliceStore = try MessageStore.open(path: ":memory:")
        let bobStore = try MessageStore.open(path: ":memory:")
        try aliceStore.upsertContact(contact: contact(for: bob, name: "Bob"))
        let group = try createGroup(name: "Family", memberUserIds: [alice.userId, bob.userId])
        try bobStore.upsertGroup(group: group)
        let now: Int64 = 1_700_000_000_000

        let envelope = try receipt(from: alice, senderStore: aliceStore, to: bob, now: now)
        XCTAssertTrue(try consumeAsEndpoint(envelope, as: bob, store: bobStore, now: now))

        let acked = try bobStore.coreRelayAckIdsWithConsumed(
            items: [seen(99, envelope.msgId, computeRecipientHint(recipientUserId: group.id, timestampMs: now))],
            ownUserId: bob.userId,
            nowMs: now
        )
        XCTAssertTrue(acked.isEmpty, "a shared group row is never ours alone to delete")
    }

    func testAChatKindIsNeverRecordedHere() throws {
        let alice = generateIdentity()
        let bob = generateIdentity()
        let aliceStore = try MessageStore.open(path: ":memory:")
        let bobStore = try MessageStore.open(path: ":memory:")
        try aliceStore.upsertContact(contact: contact(for: bob, name: "Bob"))
        let now: Int64 = 1_700_000_000_000

        let envelope = try receipt(from: alice, senderStore: aliceStore, to: bob, now: now)
        // Same envelope, declared as a chat kind: already has durable
        // evidence, so this table must not grow for it.
        XCTAssertFalse(try consumeAsEndpoint(
            envelope, as: bob, store: bobStore, kind: ProtocolKind.text, now: now
        ))
        XCTAssertEqual(try bobStore.consumedHiddenMsgIdCount(), 0)
    }

    func testRecordedEvidenceIsPrunedOnceTheEnvelopeExpires() throws {
        // Bounded by construction: the record dies with the envelope it
        // vouches for, on the same prune the relay sync pass already runs.
        let alice = generateIdentity()
        let bob = generateIdentity()
        let aliceStore = try MessageStore.open(path: ":memory:")
        let bobStore = try MessageStore.open(path: ":memory:")
        try aliceStore.upsertContact(contact: contact(for: bob, name: "Bob"))
        let now: Int64 = 1_700_000_000_000

        let envelope = try receipt(from: alice, senderStore: aliceStore, to: bob, now: now)
        XCTAssertTrue(try consumeAsEndpoint(envelope, as: bob, store: bobStore, now: now))

        // A row past its expiry reads as absent even before the prune runs.
        XCTAssertFalse(try bobStore.consumedHiddenMsgIdRecorded(
            msgId: envelope.msgId, nowMs: envelope.expiry
        ))
        XCTAssertEqual(try bobStore.pruneExpiredConsumedHiddenMsgIds(nowMs: now), 0)
        XCTAssertEqual(try bobStore.pruneExpiredConsumedHiddenMsgIds(nowMs: envelope.expiry), 1)
        XCTAssertFalse(try bobStore.consumedHiddenMsgIdRecorded(msgId: envelope.msgId, nowMs: now))
    }
}
