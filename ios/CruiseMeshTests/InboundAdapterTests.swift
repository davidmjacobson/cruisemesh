import XCTest
@testable import CruiseMesh

/// What `MeshController` executes when the rebuilt receive path handles an
/// arriving envelope.
///
/// `MeshController` itself is not unit-testable here (CoreBluetooth, live
/// sockets), so these drive the exact sequence its core path makes -- build the
/// §6.4 frame, run `processInboundFrame`, translate the outcome with
/// `InboundAdapter`, and commit -- and assert on the plan that comes back. Each
/// case is one of the receive behaviours the legacy path pins today: a 1:1
/// message we open, a blocked sender, a group message, pure mule traffic, an
/// expired envelope, and a second copy of something already handled.
///
/// The point of asserting on the plan rather than on core's record directly is
/// that the plan is the whole contract between the two: if it stops carrying a
/// re-flood frame, a commit token, or a delivered sender, the shell silently
/// stops flooding, stops acking, or stops delivering.
final class InboundAdapterTests: XCTestCase {

    private static let now: Int64 = 1_700_000_000_000
    private static let day: Int64 = 24 * 60 * 60 * 1000

    // MARK: - structural

    /// If every field of the plan is a number, a byte string, a bool, an enum
    /// or a record of those, there is no object in the shell's reach with a
    /// store handle to write through or a socket to open behind core's back.
    func testThePlanIsMadeOnlyOfValues() throws {
        let world = try World()
        let plan = try world.run(world.pairwiseFrameToMe(text: "hi"))
        let forbidden = ["MessageStore", "SeenIds", "URLSession", "URLRequest", "URL", "Socket", "MeshController"]
        for child in Mirror(reflecting: plan).children {
            let typeName = String(describing: type(of: child.value))
            XCTAssertFalse(
                forbidden.contains { typeName.contains($0) },
                "InboundExecutionPlan.\(child.label ?? "?") is a \(typeName); the plan may hold only values"
            )
        }
    }

    /// The source discriminant core's relay rules turn on. A live BLE/LAN frame
    /// arrives on a link address; a relay-fetched row has none.
    func testSourceIsRelayOnlyWhenNoLinkAddressCarriedTheFrame() {
        XCTAssertEqual(InboundAdapter.source(forSourceAddress: nil), .relay)
        XCTAssertEqual(InboundAdapter.source(forSourceAddress: "AA:BB:CC:DD:EE:FF"), .mesh)
        XCTAssertEqual(InboundAdapter.source(forSourceAddress: "192.168.1.20:41234"), .mesh)
    }

    /// The switch is off until someone turns it on, and it is the only thing
    /// that decides which engine runs.
    func testTheEngineDefaultsToLegacyAndRoundTrips() {
        InboundEngineSettings.setPathEngine(.legacy)
        XCTAssertEqual(InboundEngineSettings.pathEngine(), .legacy)
        InboundEngineSettings.setPathEngine(.core)
        XCTAssertEqual(InboundEngineSettings.pathEngine(), .core)
        InboundEngineSettings.setPathEngine(.legacy)
        XCTAssertEqual(InboundEngineSettings.pathEngine(), .legacy)
    }

    // MARK: - a 1:1 message addressed to this device

    func testAPairwiseMessageComesBackAsOnePayloadToDeliverAndOneCommitToApply() throws {
        let world = try World()
        let plan = try world.run(world.pairwiseFrameToMe(text: "see you at dinner"))

        XCTAssertEqual(plan.disposition, .consumed)
        XCTAssertEqual(plan.work.delivered, 1)
        let delivery = try XCTUnwrap(plan.delivery)
        XCTAssertEqual(delivery.senderUserId, world.sender.userId)
        let body = try decodeExtendedMessageBody(bytes: delivery.payload)
        XCTAssertEqual(body.content, Data("see you at dinner".utf8))
        // Home: nothing is flooded onward and nothing is carried.
        XCTAssertNil(plan.relayFrame)
        XCTAssertFalse(plan.carried)
        XCTAssertFalse(plan.droppedBlocked)
        // The pairwise/group discriminant the shell routes delivery on.
        XCTAssertEqual(try XCTUnwrap(delivery.commit.hiddenKind), ProtocolKind.text)
    }

    /// The DTN D4 order the shell must keep: nothing is deduped until the
    /// commit that follows a successful delivery, and then the next copy is.
    func testASecondCopyIsOnlyDedupedAfterTheDeliveryCommit() throws {
        let world = try World()
        let frame = world.pairwiseFrameToMe(text: "same message")

        let first = try world.run(frame)
        let delivery = try XCTUnwrap(first.delivery)
        // Before the commit -- the state a failed delivery leaves behind -- the
        // envelope is still re-presentable, so a retry can deliver it.
        let retry = try world.run(frame)
        XCTAssertEqual(retry.disposition, .consumed)
        XCTAssertNotNil(retry.delivery)

        world.store.coreCommitInboundDelivery(seen: world.seen, commit: delivery.commit)

        let third = try world.run(frame)
        XCTAssertEqual(third.disposition, .seen)
        XCTAssertEqual(third.work.deduped, 1)
        XCTAssertNil(third.delivery)
        XCTAssertNil(third.relayFrame)
    }

    func testABlockedSenderIsConsumedButHandsBackNothingToDeliver() throws {
        let world = try World()
        try world.store.blockUser(userId: world.sender.userId, nowMs: Self.now)

        let plan = try world.run(world.pairwiseFrameToMe(text: "let me back in"))

        // Consumed so the relay copy acks away instead of being refetched
        // forever, but no payload reaches any handler.
        XCTAssertEqual(plan.disposition, .consumed)
        XCTAssertTrue(plan.droppedBlocked)
        XCTAssertNil(plan.delivery)
        XCTAssertEqual(plan.work.dropped, 1)
        XCTAssertEqual(plan.work.delivered, 0)
    }

    // MARK: - group traffic

    func testAGroupMessageIsDeliveredWithoutHiddenKindEvidenceAndStillMuledOn() throws {
        let world = try World()
        let group = try createGroup(
            name: "Family",
            memberUserIds: [world.me.userId, world.sender.userId]
        )
        try world.store.upsertGroup(group: group)

        let plan = try world.run(world.groupFrame(group: group, from: world.sender, text: "we docked"))

        XCTAssertEqual(plan.disposition, .consumed)
        let delivery = try XCTUnwrap(plan.delivery)
        XCTAssertEqual(delivery.senderUserId, world.sender.userId)
        // No hidden-kind evidence for a group delivery: vouching for an
        // envelope is a pairwise-only licence, and the shell reads the same
        // field to know this payload belongs to the group handlers.
        XCTAssertNil(delivery.commit.hiddenKind)
        // Muled on for members who were not here.
        XCTAssertNotNil(plan.relayFrame)
        XCTAssertTrue(plan.carried)
    }

    func testAGroupMessageSignedByANonMemberIsMuledButNeverDelivered() throws {
        let world = try World()
        let outsider = generateIdentity()
        // The outsider holds the group key (a former member, or a leaked key)
        // but is no longer named in the membership.
        let group = try createGroup(
            name: "Family",
            memberUserIds: [world.me.userId, world.sender.userId]
        )
        try world.store.upsertGroup(group: group)

        let plan = try world.run(world.groupFrame(group: group, from: outsider, text: "still here"))

        XCTAssertNil(plan.delivery)
        XCTAssertEqual(plan.work.dropped, 1)
        XCTAssertEqual(plan.disposition, .consumed)
        XCTAssertNotNil(plan.relayFrame)
    }

    // MARK: - traffic that is not ours

    func testForeignTrafficIsCarriedAndFloodedOnWithOneFewerHop() throws {
        let world = try World()
        let stranger = generateIdentity()
        let plan = try world.run(world.pairwiseFrame(
            from: world.sender,
            to: stranger,
            text: "not for us",
            hopTtl: 4
        ))

        XCTAssertEqual(plan.disposition, .carried)
        XCTAssertNil(plan.delivery)
        XCTAssertTrue(plan.carried)
        let relayFrame = try XCTUnwrap(plan.relayFrame)
        guard case let .envelope(_, hopTtl, _, _, _) = try parseFrame(bytes: relayFrame) else {
            return XCTFail("the re-flood frame must still be an envelope")
        }
        XCTAssertEqual(hopTtl, 3)
    }

    func testTheLastCarrierGetsNoFrameToFloodOnward() throws {
        let world = try World()
        let stranger = generateIdentity()
        let plan = try world.run(world.pairwiseFrame(
            from: world.sender,
            to: stranger,
            text: "end of the line",
            hopTtl: 1
        ))

        XCTAssertEqual(plan.disposition, .carried)
        XCTAssertNil(plan.relayFrame)
        XCTAssertTrue(plan.carried)
    }

    func testAnExpiredEnvelopeIsDroppedWithNothingToExecute() throws {
        let world = try World()
        let plan = try world.run(world.pairwiseFrameToMe(
            text: "too late",
            expiry: Self.now - Self.day
        ))

        XCTAssertEqual(plan.disposition, .expired)
        XCTAssertEqual(plan.work.expired, 1)
        XCTAssertNil(plan.delivery)
        XCTAssertNil(plan.relayFrame)
        XCTAssertFalse(plan.carried)
    }

    // MARK: - helpers

    /// One device, its store, its seen-set, and a correspondent -- the state
    /// `MeshController` holds when a frame arrives.
    private struct World {
        let me: Identity
        let sender: Identity
        let store: MessageStore
        let seen: SeenIds

        init() throws {
            me = generateIdentity()
            sender = generateIdentity()
            store = try MessageStore.open(path: ":memory:")
            seen = SeenIds()
        }

        /// The exact call sequence `MeshController.processInboundEnvelopeViaCore`
        /// makes for a mesh-sourced frame.
        func run(_ frame: Data, source: CoreInboundSource = .mesh) throws -> InboundExecutionPlan {
            try InboundAdapter.plan(from: store.processInboundFrame(
                identity: me,
                seen: seen,
                source: source,
                frame: frame,
                nowMs: InboundAdapterTests.now
            ))
        }

        func pairwiseFrameToMe(
            text: String,
            expiry: Int64 = InboundAdapterTests.now + 7 * InboundAdapterTests.day
        ) -> Data {
            pairwiseFrame(from: sender, to: me, text: text, expiry: expiry)
        }

        func pairwiseFrame(
            from author: Identity,
            to recipient: Identity,
            text: String,
            hopTtl: UInt8 = 4,
            expiry: Int64 = InboundAdapterTests.now + 7 * InboundAdapterTests.day
        ) -> Data {
            let payload = try! encodeMessageBody(body: MessageBody(
                kind: ProtocolKind.text,
                chatId: author.userId,
                lamport: 1,
                timestamp: InboundAdapterTests.now,
                content: Data(text.utf8)
            ))
            let sealed = try! sealMessage(
                sender: author,
                recipientAgreePk: recipient.agreePk,
                payload: payload
            )
            return encodeEnvelopeFrame(
                msgId: generateMsgId(),
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: computeRecipientHint(
                    recipientUserId: recipient.userId,
                    timestampMs: InboundAdapterTests.now
                ),
                sealed: sealed
            )
        }

        func groupFrame(group: Group, from author: Identity, text: String) -> Data {
            let payload = try! encodeMessageBody(body: MessageBody(
                kind: ProtocolKind.text,
                chatId: group.id,
                lamport: 1,
                timestamp: InboundAdapterTests.now,
                content: Data(text.utf8)
            ))
            let sealed = try! sealGroupMessage(sender: author, group: group, payload: payload)
            return encodeEnvelopeFrame(
                msgId: generateMsgId(),
                hopTtl: 4,
                expiry: InboundAdapterTests.now + 7 * InboundAdapterTests.day,
                recipientHint: computeRecipientHint(
                    recipientUserId: group.id,
                    timestampMs: InboundAdapterTests.now
                ),
                sealed: sealed
            )
        }
    }
}
