import XCTest
@testable import CruiseMesh

/// What a core relay pass hands back to the shell, and what the shell does with
/// it. Mirrors Android's `CoreRelayPassProjectionTest`.
///
/// The core pass was complete long before these tests existed, and a device
/// flipped to it still went quiet: pages were fetched, rows were persisted, acks
/// were sent — and nobody was told a message had arrived, and no contact's "last
/// seen" moved. Both were shell gaps rather than core ones, which is exactly why
/// they were invisible from the core's own suite.
///
/// A real `MessageStore`, the real `CoreRelayPass`, the real `RelaySyncDriver`
/// and `RelayActionDriver`, and a scripted relay. What the delivery closure then
/// does is `MeshController`'s job and needs the whole controller, so what is
/// pinned here is the half this shell owns and can prove: that each row core
/// newly persisted is handed over exactly once, that a row it had already seen
/// is handed over never, and that a presence answer becomes the same "last seen"
/// the legacy pass computes.
final class CoreRelayPassProjectionTests: XCTestCase {

    private static let now: Int64 = 1_700_000_000_000

    // MARK: - Gap 1: delivered, and no longer silent

    func testEveryRowThePassNewlyPersistedIsHandedToTheShellOnce() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()
        relay.fetchPages = [fixture.page(ids: [3, 5])]

        let summary = fixture.run()

        XCTAssertEqual(summary.outcome, .completed)
        XCTAssertEqual(summary.rowsIngested, 2, "core must have persisted the rows itself")
        XCTAssertEqual(
            fixture.delivered.count, 2,
            "every newly persisted row must reach the inbound path, or nothing raises a notification for it"
        )
    }

    func testARowThePassHasAlreadyIngestedIsNotHandedOverAgain() throws {
        // The mailbox re-offers rows this device deliberately never acked. A
        // second delivery is a second notification for one message, which is the
        // failure on the other side of the release gate.
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()
        relay.fetchPages = [fixture.page(ids: [3])]
        _ = fixture.run()

        // The same envelope again, higher up the mailbox.
        relay.fetchPages = [fixture.page(ids: [3], at: 9)]
        _ = fixture.run()

        XCTAssertEqual(fixture.delivered.count, 1, "handed over once, not twice")
    }

    // MARK: - Gap 2: presence, projected

    func testAPresenceAnswerMovesTheContactsLastSeen() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()
        relay.presenceBody = fixture.presencePageForPeer(ageMs: 5_000)

        _ = fixture.run()

        XCTAssertEqual(
            fixture.presence.count, 1,
            "a mailbox answer about a contact's hint must resolve to that contact"
        )
        XCTAssertEqual(fixture.presence.first?.0, fixture.peerUserId)
        XCTAssertEqual(
            fixture.presence.first?.1, Self.now - 5_000,
            "the age is subtracted from this device's clock, never the relay's"
        )
    }

    func testARelayWithNothingToSayLeavesTheLastSeenAlone() throws {
        // An empty answer is a real answer, and it is not evidence of absence:
        // it must not invent a sighting.
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let fixture = try Fixture()

        _ = fixture.run()

        XCTAssertTrue(fixture.presence.isEmpty, "no answer must not become a sighting")
    }

    // MARK: - Gap 3: two brakes, not one

    func testMailForASilentEndpointStaysQueuedRatherThanTakingTheFallback() throws {
        // Silence declines the upload; a rejection takes this device's own
        // mailbox as the fallback. Folded into one flag, the first behaved like
        // the second — and the marker that misroute writes is terminal.
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let silent = try Fixture(contactEndpointAnswering: false)
        try silent.queueMessageToPeer()

        _ = silent.run()

        XCTAssertEqual(relay.posts, 0, "a quiet host earns no post at all")
        XCTAssertEqual(try silent.pendingOutbound(), 1, "and the message stays queued for a later pass")
    }

    func testMailForARefusedEndpointFallsBackToOurOwnMailbox() throws {
        let relay = CoreRelayFakeRelay()
        relay.install()
        defer { relay.remove() }
        let refused = try Fixture(contactEndpointUsable: false)
        try refused.queueMessageToPeer()

        _ = refused.run()

        XCTAssertEqual(relay.posts, 1, "a refusal falls back to our own mailbox")
    }

    // MARK: - Harness

    /// A store with an identity, a contact and this device's own mailbox, with
    /// the projection wired to recorders instead of to the controller.
    private final class Fixture {
        private let baseUrl = "https://relay.test"
        private let contactEndpointUsable: Bool
        private let contactEndpointAnswering: Bool
        private let identity = generateIdentity()
        private let peer = generateIdentity()
        private let store: MessageStore
        private let contact: Contact

        /// Envelopes the projection handed to the inbound path.
        private(set) var delivered: [Data] = []
        /// `(userId, seenAtMs)` the projection merged.
        private(set) var presence: [(Data, Int64)] = []

        var peerUserId: Data { peer.userId }

        init(contactEndpointUsable: Bool = true, contactEndpointAnswering: Bool = true) throws {
            self.contactEndpointUsable = contactEndpointUsable
            self.contactEndpointAnswering = contactEndpointAnswering
            store = try MessageStore.open(path: ":memory:")
            contact = Contact(
                userId: peer.userId,
                name: "Peer",
                signPk: peer.signPk,
                agreePk: peer.agreePk,
                relayUrl: nil,
                relayToken: nil,
                nickname: nil
            )
            try store.upsertContact(contact: contact)
        }

        private func ownHint() -> Data {
            computeRecipientHint(recipientUserId: identity.userId, timestampMs: Self.now)
        }

        /// A page of rows addressed to this device that it has never seen.
        func page(ids: [Int64], at rowId: Int64? = nil) -> String {
            let hint = ownHint()
            let expiry = Self.now + 6 * 24 * 60 * 60 * 1000
            let rows = ids.map { id -> String in
                var msgId = Data(repeating: 0, count: 16)
                msgId[0] = UInt8(truncatingIfNeeded: id)
                msgId[8] = 0xA5
                let sealed = Data(repeating: UInt8(truncatingIfNeeded: id), count: 96)
                return "{\"id\":\(rowId ?? id),\"msg_id\":\"\(relayBase64Url(msgId))\",\"hop_ttl\":3,"
                    + "\"recipient_hint\":\"\(relayBase64Url(hint))\","
                    + "\"sealed\":\"\(relayBase64Url(sealed))\",\"expiry_ms\":\(expiry)}"
            }.joined(separator: ",")
            return "{\"envelopes\":[\(rows)],\"next_cursor\":\(rowId ?? ids.last ?? 0)}"
        }

        func presencePageForPeer(ageMs: Int64) -> String {
            let hint = recentPresenceHintsFor(userId: peer.userId, nowMs: Self.now)[0]
            return "{\"now_ms\":\(Self.now),\"presence\":[{\"hint\":\"\(relayBase64Url(hint))\","
                + "\"last_seen_ms\":\(Self.now - ageMs)}]}"
        }

        func queueMessageToPeer() throws {
            _ = try store.authorPairwiseMessage(
                identity: identity,
                contact: contact,
                kind: 1,
                payload: Data("hello".utf8),
                replyToMsgId: nil,
                timestampMs: Self.now
            )
        }

        func pendingOutbound() throws -> Int {
            try store.pendingRelayOutboundEnvelopes(limit: 64, nowMs: Self.now, skipRecipientUserIds: []).count
        }

        func run() -> CoreRelayPassSummary {
            let projector = CoreRelayPassProjector(
                deliver: { [weak self] envelope, _ in self?.delivered.append(envelope.msgId) },
                mergePresence: { [weak self] userId, seenAtMs in
                    self?.presence.append((userId, seenAtMs))
                }
            )
            let plan = CoreRelayPassPlan(
                own: CoreRelayEndpointConfig(url: baseUrl, token: "member-token"),
                contacts: [
                    CoreRelayContactConfig(
                        userId: contact.userId,
                        relayUrl: contact.relayUrl,
                        relayToken: contact.relayToken,
                        endpointUsable: contactEndpointUsable,
                        endpointAnswering: contactEndpointAnswering
                    )
                ],
                ownUserId: identity.userId,
                fetchHints: [ownHint()],
                presenceAnnounce: [],
                presenceQuery: recentPresenceHintsFor(userId: peer.userId, nowMs: Self.now),
                ownEndpointChanged: false,
                sweptThisSession: true,
                consecutiveRateLimits: 0,
                quietUntilMs: 0,
                budgets: coreRelayPassDefaultBudgets()
            )
            return RelaySyncDriver(
                store: store,
                executor: LiveRelayActionExecutor(),
                clock: { Self.now },
                onProjection: { [weak self] projection in
                    guard let self else { return }
                    projector.project(
                        projection,
                        identity: self.identity,
                        contacts: [self.contact],
                        nowMs: Self.now
                    )
                }
            ).run(plan: plan, passId: "t")
        }

        private static var now: Int64 { CoreRelayPassProjectionTests.now }
    }
}
