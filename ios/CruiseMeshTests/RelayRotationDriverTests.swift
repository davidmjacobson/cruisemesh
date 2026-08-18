import XCTest
@testable import CruiseMesh

/// §10 step 2's driver, against a real core and a relay that answers whatever
/// the test needs it to.
///
/// The properties pinned here are the ones a rotation cannot be allowed to
/// lose, in the order they would hurt:
///
/// 1. **The journal is written before the call.** A client that asked first and
///    wrote afterwards would, on one dropped response, hold neither credential
///    and lock a family out of its own mailbox.
/// 2. **No answer, no commit.** The saved credential moves only when the relay
///    has confirmed the re-key.
/// 3. **An unreachable relay does not lose the rotation.** The removal is
///    already done; the rotation waits in the journal and a later pass — in a
///    later process, if it comes to that — performs it.
/// 4. **Nothing hot-loops.** There is a rate-limit incident in this codebase's
///    history, and the rotate route is the one a family needs on the day a
///    phone is stolen.
///
/// The Swift twin of Android's `RelayRotationDriverTest`.
final class RelayRotationDriverTests: XCTestCase {
    private let nowMs: Int64 = 1_755_000_000_000
    private let relayUrl = "https://relay.example"
    private let oldToken = "family-token-from-before-the-removal"

    // MARK: - Fixtures

    /// One person, two phones, and the revocation that buries the second.
    private struct Fleet {
        let identity: Identity
        let approver: DeviceKeypair
        let sibling: DeviceKeypair
        let store: MessageStore
        let revocation: RevocationCommit

        static func revoked(nowMs: Int64) throws -> Fleet {
            let identity = generateIdentity()
            let approver = generateDeviceKeypair()
            let sibling = generateDeviceKeypair()
            let genesis = try coreLinkGenesisRoster(
                personRootSignSk: identity.signSk,
                deviceSignPk: approver.signPk,
                deviceAgreePk: approver.agreePk
            )
            let roster = try coreLinkSignNewDeviceRoster(
                current: genesis,
                personRootSignPk: identity.signPk,
                approvingDeviceSignSk: approver.signSk,
                newDeviceSignPk: sibling.signPk,
                newDeviceAgreePk: sibling.agreePk
            ).roster
            let store = try MessageStore.open(path: ":memory:")
            try store.adoptOwnRoster(
                roster: roster,
                personRootSignPk: identity.signPk,
                ownDeviceId: approver.deviceId
            )
            _ = try store.coreSetOwnSyncContext(
                roster: roster,
                inboxKeyGeneration: roster.inboxKeyGeneration
            )
            // Generation 0 is the deployed person agreement key (§10 note 4).
            let key = InboxKey(generation: 0, agreePk: identity.agreePk, agreeSk: identity.agreeSk)
            let update = try coreRevokeDevicesRoster(
                current: roster,
                personRootSignPk: identity.signPk,
                approvingDeviceSignSk: approver.signSk,
                revokedDeviceIds: [sibling.deviceId],
                currentInboxKey: key
            )
            _ = try store.beginOwnRevocation(
                update: update,
                personRootSignPk: identity.signPk,
                ownDevice: approver,
                nowMs: nowMs
            )
            let commit = try store.commitOwnRevocation(
                update: update,
                personRootSignPk: identity.signPk,
                ownDevice: approver,
                supersededInboxKey: key,
                nowMs: nowMs
            )
            return Fleet(
                identity: identity,
                approver: approver,
                sibling: sibling,
                store: store,
                revocation: commit
            )
        }
    }

    private final class SavedPass: RelayRotationCredential {
        var config: RelayConfig?
        var epochValue: Int64 = 0
        var adoptions = 0

        init(_ config: RelayConfig?) {
            self.config = config
        }

        func current() -> RelayConfig? { config }

        func epoch() -> Int64 { epochValue }

        func adopt(_ config: RelayConfig) {
            self.config = config
            // T23: adopting is an endpoint change, and the epoch climbs --
            // which is what makes the next pass fan the new deposit token out.
            epochValue += 1
            adoptions += 1
        }
    }

    /// A relay that answers however the test says, and counts being asked.
    private final class Relay {
        var bearers: [String] = []
        let answer: (String, Int) throws -> Data

        init(_ answer: @escaping (String, Int) throws -> Data) {
            self.answer = answer
        }

        func rotate(config: RelayConfig, body: Data) throws -> Data {
            bearers.append(config.relayToken)
            return try answer(config.relayToken, bearers.count)
        }
    }

    private func rotatedBody(token: String, envelopesMoved: Int = 0, rotated: Bool = true) -> Data {
        let deposit = relayDepositTokenFor(memberToken: token)
        let json = """
            {"family_token":"\(token)","deposit_token":"\(deposit)",\
            "envelopes_moved":\(envelopesMoved),"rotated":\(rotated)}
            """
        return Data(json.utf8)
    }

    // MARK: - Tests

    func testTheRotationIsWrittenDownBeforeTheCallAndCommittedOnlyAfterIt() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        var pendingWhenAsked: String?
        let store = fleet.store
        let relay = Relay { [self] bearer, _ in
            // The journal must already name the replacement by the time the
            // relay is asked; that row is the only thing that survives a lost
            // answer.
            pendingWhenAsked = try? store.pendingRelayRotation()?.newToken
            XCTAssertEqual(bearer, oldToken, "the retired credential is presented first")
            return rotatedBody(token: pendingWhenAsked ?? "", envelopesMoved: 7)
        }
        var nudges = 0
        let driver = RelayRotationDriver(
            store: store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: { nudges += 1 },
            pacer: RelayRotationPacer(),
            clock: { self.nowMs }
        )

        XCTAssertTrue(driver.begin(revocation: fleet.revocation))
        let planned = try XCTUnwrap(
            try store.pendingRelayRotation(),
            "the removal wrote the rotation down without any network"
        )
        XCTAssertEqual(planned.supersededToken, oldToken)
        XCTAssertEqual(
            pass.config?.relayToken,
            oldToken,
            "the credential does not move until the relay confirms"
        )

        let outcome = driver.rotateIfPending(identity: fleet.identity)

        XCTAssertEqual(outcome, .rotated(envelopesMoved: 7, alreadyDone: false))
        XCTAssertEqual(pendingWhenAsked, planned.newToken)
        XCTAssertEqual(pass.config?.relayToken, planned.newToken)
        XCTAssertEqual(pass.config?.relayUrl, relayUrl)
        XCTAssertEqual(pass.adoptions, 1)
        XCTAssertEqual(nudges, 1, "the endpoint change has to reach contacts")
        XCTAssertNil(try store.pendingRelayRotation(), "a committed rotation is not re-run")
        // §10.2's own-device leg: the replacement is in the shared settings for
        // a sibling that slept through the ceremony.
        XCTAssertEqual(try store.relayCredentialSetting()?.token, planned.newToken)
    }

    func testARemovalWithNoReachableRelayStillRemovesAndTheRotationWaits() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let store = fleet.store
        var reachable = false
        let relay = Relay { [self] _, _ in
            guard reachable else { throw URLError(.notConnectedToInternet) }
            return rotatedBody(token: (try? store.pendingRelayRotation()?.newToken) ?? "")
        }
        var now = nowMs
        let offline = RelayRotationDriver(
            store: store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { now }
        )

        XCTAssertTrue(offline.begin(revocation: fleet.revocation))
        guard case .deferred(let step) = offline.rotateIfPending(identity: fleet.identity),
              case .retry = step else {
            return XCTFail("an unreachable relay is a wait, not a failure")
        }
        let planned = try XCTUnwrap(
            try store.pendingRelayRotation(),
            "an unreachable relay must not lose the rotation"
        )
        XCTAssertEqual(pass.config?.relayToken, oldToken)

        // A later process, on a phone that has since found internet. The pacer
        // is per-process and the journal is not, which is exactly the split
        // this asserts: nothing about the retry depends on remembering.
        reachable = true
        now += 60_000
        let relaunched = RelayRotationDriver(
            store: store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { now }
        )
        let outcome = relaunched.rotateIfPending(identity: fleet.identity)

        XCTAssertEqual(outcome, .rotated(envelopesMoved: 0, alreadyDone: false))
        XCTAssertEqual(pass.config?.relayToken, planned.newToken)
        XCTAssertNil(try store.pendingRelayRotation())
    }

    func testARefusedCallNeverCommits() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let relay = Relay { _, _ in
            throw RelayHTTPError(statusCode: 500, relayCode: nil, responseBody: "having a day")
        }
        let driver = RelayRotationDriver(
            store: fleet.store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { self.nowMs }
        )

        driver.begin(revocation: fleet.revocation)
        guard case .deferred = driver.rotateIfPending(identity: fleet.identity) else {
            return XCTFail("a refused call is a wait")
        }

        XCTAssertNotNil(try fleet.store.pendingRelayRotation())
        XCTAssertEqual(pass.config?.relayToken, oldToken)
        XCTAssertEqual(pass.adoptions, 0)
        XCTAssertNil(
            try fleet.store.relayCredentialSetting(),
            "nothing was announced to the siblings either"
        )
    }

    /// The recovery case the whole two-token design exists for: the rotation
    /// landed, the answer did not come back, and the device wakes up holding a
    /// credential the server has already retired.
    func testACredentialTheRelayHasAlreadyRetiredIsConfirmedUnderTheReplacement() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let store = fleet.store
        let relay = Relay { [self] bearer, attempt in
            if attempt == 1 {
                XCTAssertEqual(bearer, oldToken)
                throw RelayHTTPError(
                    statusCode: 401,
                    relayCode: nil,
                    responseBody: "unknown family token"
                )
            }
            // relayd answers a repeat presentation with the same values and
            // `rotated: false`, which is a success, not a failure.
            XCTAssertEqual(bearer, (try? store.pendingRelayRotation()?.newToken) ?? "")
            return rotatedBody(token: bearer, rotated: false)
        }
        let driver = RelayRotationDriver(
            store: store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { self.nowMs }
        )

        driver.begin(revocation: fleet.revocation)
        let planned = try XCTUnwrap(try store.pendingRelayRotation())
        let outcome = driver.rotateIfPending(identity: fleet.identity)

        XCTAssertEqual(outcome, .rotated(envelopesMoved: 0, alreadyDone: true))
        XCTAssertEqual(relay.bearers, [oldToken, planned.newToken])
        XCTAssertEqual(pass.config?.relayToken, planned.newToken)
        XCTAssertNil(try store.pendingRelayRotation())
    }

    func testARateLimitedRotationWaitsOutTheWindowInsteadOfHammeringIt() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let relay = Relay { _, _ in
            throw RelayHTTPError(
                statusCode: 429,
                relayCode: "rate_limited",
                responseBody: "too fast",
                retryAfter: "60"
            )
        }
        let pacer = RelayRotationPacer()
        var now = nowMs
        let driver = RelayRotationDriver(
            store: fleet.store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: pacer,
            clock: { now }
        )

        driver.begin(revocation: fleet.revocation)
        driver.rotateIfPending(identity: fleet.identity)
        XCTAssertEqual(relay.bearers.count, 1)

        // Every pass for the next several minutes finds the rotation owed and
        // makes no request. This is the behaviour a rerun loop that ignored
        // Retry-After once cost a family ~290 posts a minute for.
        for _ in 0..<8 {
            now += 60_000
            guard case .waiting = driver.rotateIfPending(identity: fleet.identity) else {
                return XCTFail("a rotation inside its quiet window must not be retried")
            }
        }
        XCTAssertEqual(relay.bearers.count, 1)
        XCTAssertNotNil(try fleet.store.pendingRelayRotation(), "and it is still owed")

        // Past the window, it is tried again -- once.
        now = pacer.nextAttemptAtMs
        driver.rotateIfPending(identity: fleet.identity)
        XCTAssertEqual(relay.bearers.count, 2)
    }

    func testARelayThatCannotReKeyFromADeviceIsNotAskedForever() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let relay = Relay { _, _ in
            throw RelayHTTPError(
                statusCode: 409,
                relayCode: "rotation_unsupported",
                responseBody: "configured on the server"
            )
        }
        var now = nowMs
        let driver = RelayRotationDriver(
            store: fleet.store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { now }
        )

        driver.begin(revocation: fleet.revocation)
        XCTAssertEqual(
            driver.rotateIfPending(identity: fleet.identity),
            .gaveUp(.serverManagedToken)
        )
        // Cleared, so the next pass does not ask again. The person keeps the
        // credential they have -- and so, honestly, does the removed device.
        XCTAssertNil(try fleet.store.pendingRelayRotation())
        XCTAssertEqual(pass.config?.relayToken, oldToken)
        now += 3_600_000
        XCTAssertEqual(driver.rotateIfPending(identity: fleet.identity), .nothingPending)
        XCTAssertEqual(relay.bearers.count, 1)
    }

    func testAPersonWithNoShorePassHasNothingToRotate() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(nil)
        let relay = Relay { _, _ in
            XCTFail("no pass, no call")
            return Data()
        }
        let driver = RelayRotationDriver(
            store: fleet.store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { self.nowMs }
        )

        XCTAssertFalse(driver.begin(revocation: fleet.revocation))
        XCTAssertNil(try fleet.store.pendingRelayRotation())
        XCTAssertEqual(driver.rotateIfPending(identity: fleet.identity), .nothingPending)
    }

    /// §10.2's own-device leg from the other end: a sibling that was asleep
    /// through the ceremony reads the replacement out of the shared settings and
    /// writes it down.
    ///
    /// Driven here through a store that has the setting in it, because the
    /// transport that would deliver it does not exist on either shell yet. What
    /// the test is really pinning is the guard around the adoption, since that
    /// is what will still be load-bearing when the transport lands.
    func testASiblingAdoptsAnAnnouncedCredentialOnlyOnItsOwnRelay() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let driver = RelayRotationDriver(
            store: fleet.store,
            credential: pass,
            rotate: { _, _ in XCTFail("adopting makes no calls"); return Data() },
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { self.nowMs }
        )

        // Nothing announced: nothing changes.
        driver.adoptAnnouncedCredential()
        XCTAssertEqual(pass.config?.relayToken, oldToken)

        // The rotating sibling's announcement, as it lands in the settings.
        driver.begin(revocation: fleet.revocation)
        let planned = try XCTUnwrap(try fleet.store.pendingRelayRotation())
        _ = try fleet.store.commitRelayRotation(plan: planned, nowMs: nowMs)
        // Put this device back where a sibling that missed the ceremony is:
        // holding the retired credential, with the replacement announced.
        pass.config = RelayConfig(relayUrl: relayUrl, relayToken: oldToken)

        driver.adoptAnnouncedCredential()
        XCTAssertEqual(pass.config?.relayToken, planned.newToken)

        // A phone on a different relay is not the family's to move, and a phone
        // whose person removed the pass must not have one reinstalled.
        pass.config = RelayConfig(relayUrl: "https://relay.somewhere-else.example", relayToken: oldToken)
        driver.adoptAnnouncedCredential()
        XCTAssertEqual(pass.config?.relayToken, oldToken)
        pass.config = nil
        driver.adoptAnnouncedCredential()
        XCTAssertNil(pass.config)
    }

    /// A second removal while a rotation is still in flight must not re-mint.
    /// The pending row may already name the credential the server moved to, and
    /// overwriting it would throw away the only record of it — locking the
    /// family out of its own mailbox to lock one thief out.
    func testASecondRemovalLetsTheRotationAlreadyInFlightFinish() throws {
        let fleet = try Fleet.revoked(nowMs: nowMs)
        let pass = SavedPass(RelayConfig(relayUrl: relayUrl, relayToken: oldToken))
        let relay = Relay { _, _ in throw URLError(.notConnectedToInternet) }
        let driver = RelayRotationDriver(
            store: fleet.store,
            credential: pass,
            rotate: relay.rotate,
            onRotated: {},
            pacer: RelayRotationPacer(),
            clock: { self.nowMs }
        )

        driver.begin(revocation: fleet.revocation)
        let first = try XCTUnwrap(try fleet.store.pendingRelayRotation()).newToken

        XCTAssertTrue(driver.begin(revocation: fleet.revocation))
        XCTAssertEqual(try fleet.store.pendingRelayRotation()?.newToken, first)
    }
}

/// How often §10 step 2's rotate call may be made. Plain class, plain test.
final class RelayRotationPacerTests: XCTestCase {
    private let now: Int64 = 1_755_000_000_000

    func testAFreshPacerAllowsTheFirstAttemptAtOnce() {
        let pacer = RelayRotationPacer()
        XCTAssertTrue(pacer.mayAttempt(nowMs: now))
        XCTAssertEqual(pacer.consecutiveFailures, 0)
    }

    func testAFailureHoldsTheNextAttemptOffForExactlyWhatItWasGiven() {
        let pacer = RelayRotationPacer()
        pacer.onFailure(nowMs: now, delayMs: 900_000)

        XCTAssertFalse(pacer.mayAttempt(nowMs: now))
        XCTAssertFalse(pacer.mayAttempt(nowMs: now + 899_999))
        XCTAssertTrue(pacer.mayAttempt(nowMs: now + 900_000))
        XCTAssertEqual(pacer.consecutiveFailures, 1)
    }

    /// A second failure inside an open window must not become permission to ask
    /// sooner. Same rule as the relay pass's own quiet window, and for the same
    /// reason: the shortest wait must never win.
    func testALaterShorterWaitCannotPullAnOpenWindowIn() {
        let pacer = RelayRotationPacer()
        pacer.onFailure(nowMs: now, delayMs: 3_600_000)
        pacer.onFailure(nowMs: now + 1_000, delayMs: 30_000)

        XCTAssertFalse(pacer.mayAttempt(nowMs: now + 60_000))
        XCTAssertEqual(pacer.nextAttemptAtMs, now + 3_600_000)
        XCTAssertEqual(pacer.consecutiveFailures, 2)
    }

    func testSettlingClearsTheLadderSoTheNextCeremonyStartsFromTheBottom() {
        let pacer = RelayRotationPacer()
        pacer.onFailure(nowMs: now, delayMs: 3_600_000)
        pacer.onSettled()

        XCTAssertTrue(pacer.mayAttempt(nowMs: now))
        XCTAssertEqual(pacer.consecutiveFailures, 0)
    }

    func testANegativeDelayIsNotAWayToAskSooner() {
        let pacer = RelayRotationPacer()
        pacer.onFailure(nowMs: now, delayMs: -5_000)
        XCTAssertTrue(pacer.mayAttempt(nowMs: now))
    }
}
