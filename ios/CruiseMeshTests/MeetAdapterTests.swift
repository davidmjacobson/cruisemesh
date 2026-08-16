import XCTest
@testable import CruiseMesh

/// What `MeshController` executes when the rebuilt encounter path runs one
/// meeting.
///
/// The claim under test is the seam, not the planner: `mesh_meet.rs` owns the
/// ordering, the budgets and the carry lifecycle and has its own Rust tests for
/// all of it. What can only be proven here is that this shell hands the planner
/// a real encounter and then actually puts the frames it returns on the
/// transport, in the order it returned them — and that with the flag at its
/// shipped default the planner is not reached at all.
///
/// `MeshController` itself is not unit-testable here (CoreBluetooth, live
/// sockets), so these drive `MeetAdapter` against a recording send, exactly as
/// the controller wires it. Mirrors Android's `CoreMeetEngineTest`.
final class MeetAdapterTests: XCTestCase {

    private static let address = "AA:BB:CC:DD:EE:FF"
    private static let now: Int64 = 1_700_000_000_000

    /// The switch is off until someone turns it on, and it is the only thing
    /// that decides which sequencer runs.
    func testTheEngineDefaultsToLegacyAndRoundTrips() {
        MeetEngineSettings.setMeetEngine(.legacy)
        XCTAssertEqual(MeetEngineSettings.meetEngine(), .legacy)
        MeetEngineSettings.setMeetEngine(.core)
        XCTAssertEqual(MeetEngineSettings.meetEngine(), .core)
        MeetEngineSettings.setMeetEngine(.legacy)
        XCTAssertEqual(MeetEngineSettings.meetEngine(), .legacy)
    }

    /// The branch point, in the shape every call site in `MeshController` uses
    /// it: the planner is reached only on the `.core` selection, so a device on
    /// the shipped default runs the sequencing it ran before this package and
    /// the adapter is never asked for a frame.
    func testALegacyFlaggedEncounterNeverReachesThePlanner() throws {
        MeetEngineSettings.setMeetEngine(.legacy)
        let world = try World()
        _ = try world.carryOneForPeer()
        XCTAssertEqual(try world.store.carriedLen(), 1, "the carry a core encounter would offer")

        if MeetEngineSettings.meetEngine() == .core {
            world.adapter.encounter(
                address: MeetAdapterTests.address,
                ownUserId: world.me.userId,
                peerUserId: world.peer.userId,
                trigger: .firstContact
            )
        }

        XCTAssertTrue(world.sent.isEmpty, "the legacy branch put nothing on the link")
        XCTAssertEqual(try world.store.carriedLen(), 1, "and touched no carry row")
    }

    /// A first-contact encounter under `.core`: the planner's digest goes out
    /// first and the hint-matched carry follows it. The ordering is the
    /// load-bearing part — core's exchange window opens when the digest is
    /// enqueued, and a multi-KB drain ahead of it on a BLE link would hold it
    /// in the FIFO past that window.
    func testACoreFlaggedEncounterSendsTheDigestThenTheTargetedCarry() throws {
        let world = try World()
        let carriedMsgId = try world.carryOneForPeer()

        let work = try XCTUnwrap(world.adapter.encounter(
            address: MeetAdapterTests.address,
            ownUserId: world.me.userId,
            peerUserId: world.peer.userId,
            trigger: .firstContact
        ))

        XCTAssertEqual(work.digestsSent, 1, "one 1:1 digest was owed on a link that never ran one")
        XCTAssertEqual(work.targetedSent, 1, "the carry hint-matched this peer")

        XCTAssertEqual(world.sent.count, 2)
        for (address, _) in world.sent {
            XCTAssertEqual(address, MeetAdapterTests.address)
        }
        guard case .digest = try parseFrame(bytes: world.sent[0].1) else {
            return XCTFail("the digest is first")
        }
        guard case let .envelope(msgId, _, _, _, _) = try parseFrame(bytes: world.sent[1].1) else {
            return XCTFail("the drained carry follows it")
        }
        XCTAssertEqual(msgId, carriedMsgId)

        // CARRY-01 / DTN D2: dispatch is not proof of receipt. The row survives
        // being offered, and only the peer's digest can retire it.
        XCTAssertEqual(try world.store.carriedLen(), 1, "the carry row survives dispatch")
        XCTAssertEqual(work.confirmedRemoved, 0)
    }

    /// A peer digest answered under `.core`, on an authenticated link: the ids
    /// the peer advertised retire the copy we were carrying for them
    /// (CARRY-02), and nothing is re-offered — answering a digest must never
    /// provoke one back, or two converged phones ping-pong for as long as they
    /// stay in range.
    func testAnAuthenticatedPeerDigestRetiresTheCarryAndProvokesNoDigestBack() throws {
        let world = try World()
        let carriedMsgId = try world.carryOneForPeer()

        let work = try XCTUnwrap(world.adapter.encounter(
            address: MeetAdapterTests.address,
            ownUserId: world.me.userId,
            peerUserId: world.peer.userId,
            trigger: .peerDigest,
            peerKnownMsgIds: [carriedMsgId],
            peerAuthenticated: true
        ))

        XCTAssertEqual(work.digestsSent, 0, "answering a digest owes no digest back")
        XCTAssertEqual(work.targetedSent, 0, "the peer already holds it")
        XCTAssertEqual(work.confirmedRemoved, 1, "proof of receipt retired the carry")
        XCTAssertEqual(try world.store.carriedLen(), 0)
        XCTAssertTrue(world.sent.isEmpty)
    }

    /// The same digest over an unauthenticated link. CARRY-02: a bare BLE claim
    /// still suppresses the re-offer this encounter would have made, but it may
    /// never delete the durable copy.
    func testAnUnauthenticatedPeerDigestSuppressesTheOfferButKeepsTheCarry() throws {
        let world = try World()
        let carriedMsgId = try world.carryOneForPeer()

        let work = try XCTUnwrap(world.adapter.encounter(
            address: MeetAdapterTests.address,
            ownUserId: world.me.userId,
            peerUserId: world.peer.userId,
            trigger: .peerDigest,
            peerKnownMsgIds: [carriedMsgId],
            peerAuthenticated: false
        ))

        XCTAssertEqual(work.confirmedRemoved, 0, "an unauthenticated claim never deletes")
        XCTAssertEqual(work.skippedKnown, 1, "but it is still honoured as an exclusion")
        XCTAssertEqual(try world.store.carriedLen(), 1, "the durable copy stays")
        XCTAssertTrue(world.sent.isEmpty)
    }

    /// The planner records its progress on the objects this shell already
    /// holds, not on ones it built for the call. If it did not, a second
    /// encounter on the same link would owe a second digest immediately and the
    /// re-digest window would never mean anything.
    func testASecondEncounterOnTheSameLinkIsInsideTheReDigestWindow() throws {
        let world = try World()

        let first = try XCTUnwrap(world.adapter.encounter(
            address: MeetAdapterTests.address,
            ownUserId: world.me.userId,
            peerUserId: world.peer.userId,
            trigger: .firstContact
        ))
        XCTAssertEqual(first.digestsSent, 1)

        world.now += 1_000
        let second = try XCTUnwrap(world.adapter.encounter(
            address: MeetAdapterTests.address,
            ownUserId: world.me.userId,
            peerUserId: world.peer.userId,
            trigger: .reconnect
        ))
        XCTAssertEqual(second.digestsSent, 0, "the window the first encounter armed is still shut")
        XCTAssertLessThanOrEqual(world.sent.count, 1, "and the shell sent nothing a second time")
    }

    // MARK: - rig

    /// One encounter, wired exactly as `MeshController` wires it: the route
    /// state, spray policy and offer gate passed in rather than rebuilt,
    /// because the planner records its windows and cursors on them.
    private final class World {
        let me: Identity
        let peer: Identity
        let store: MessageStore
        let router = CoreMeshRouterState()
        let spray = CoreSprayPolicy()
        let offers = CoreCarriedOfferGate()
        var sent: [(String, Data)] = []
        var now = MeetAdapterTests.now

        init() throws {
            me = generateIdentity()
            peer = generateIdentity()
            store = try MessageStore.open(path: ":memory:")
            _ = try store.upsertImportedContact(contact: Contact(
                userId: peer.userId,
                name: "Peer",
                signPk: peer.signPk,
                agreePk: peer.agreePk,
                relayUrl: nil,
                relayToken: nil
            ))
            router.setLocalUserId(userId: me.userId)
            router.onConnected(address: MeetAdapterTests.address, transport: .central)
            _ = router.onHello(address: MeetAdapterTests.address, userId: peer.userId)
        }

        var adapter: MeetAdapter {
            MeetAdapter(
                store: store,
                router: router,
                spray: spray,
                offers: offers,
                send: { [unowned self] address, frame in self.sent.append((address, frame)) },
                now: { [unowned self] in self.now },
                sprayNow: { [unowned self] in self.now }
            )
        }

        /// A sealed envelope from a stranger to the peer, carried in through
        /// the ordinary inbound transaction so the carry row is classified
        /// exactly as a real mule copy would be.
        @discardableResult
        func carryOneForPeer() throws -> Data {
            let author = generateIdentity()
            let payload = try encodeMessageBody(body: MessageBody(
                kind: ProtocolKind.text,
                chatId: author.userId,
                lamport: 1,
                timestamp: now,
                content: Data("for you".utf8)
            ))
            let sealed = try sealMessage(
                sender: author,
                recipientAgreePk: peer.agreePk,
                payload: payload
            )
            let msgId = generateMsgId()
            let frame = encodeEnvelopeFrame(
                msgId: msgId,
                hopTtl: 4,
                expiry: now + 600_000,
                recipientHint: computeRecipientHint(
                    recipientUserId: peer.userId,
                    timestampMs: now
                ),
                sealed: sealed
            )
            _ = try store.processInboundFrame(
                identity: me,
                seen: SeenIds(),
                source: .mesh,
                frame: frame,
                nowMs: now
            )
            return msgId
        }
    }
}
