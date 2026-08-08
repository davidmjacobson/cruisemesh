import CoreBluetooth
import XCTest
@testable import CruiseMesh

/// The shell-side half of the Connection details page: the signal mapping, the
/// two pure UI policies (freshness bucketing and reload coalescing), and the
/// assembly of the core's answers into a view state.
///
/// The interpretation itself is tested in Rust (`core/src/connection_health.rs`).
/// What is checked here is that this platform hands the core the right facts
/// and renders back exactly what it was told -- mirrors
/// `ConnectionDetailsLogicTest.kt` on Android.
final class ConnectionDetailsLogicTests: XCTestCase {
    /// A fixed instant, so every bucket boundary in here is exact.
    private static let fixedNowMs: Int64 = 1_760_000_000_000
    private static let oneMinuteMs: Int64 = 60_000
    private static let oneHourMs: Int64 = 60 * 60_000

    private var now: Int64 { Self.fixedNowMs }
    private var minute: Int64 { Self.oneMinuteMs }
    private var hour: Int64 { Self.oneHourMs }

    // MARK: - Platform signals -> core inputs

    func testRuntimeStatesMapOneForOne() {
        XCTAssertEqual(
            ConnectionInputs.runtime(.stopped, bluetooth: .available),
            CoreMeshRuntime.stopped
        )
        XCTAssertEqual(
            ConnectionInputs.runtime(.starting, bluetooth: .available),
            CoreMeshRuntime.starting
        )
        XCTAssertEqual(
            ConnectionInputs.runtime(.meshing(nearby: 2), bluetooth: .available),
            CoreMeshRuntime.active
        )
        // One relay pass in flight is a running service, not a mode of its own.
        XCTAssertEqual(
            ConnectionInputs.runtime(.syncingViaRelay, bluetooth: .available),
            CoreMeshRuntime.active
        )
    }

    func testARunningServiceWithTheRadioOffIsBluetoothOff() {
        XCTAssertEqual(
            ConnectionInputs.runtime(.meshing(nearby: 0), bluetooth: .off),
            CoreMeshRuntime.bluetoothOff
        )
        XCTAssertEqual(
            ConnectionInputs.bluetooth(.meshing(nearby: 0), availability: .off),
            CoreDirectPathState.off
        )
    }

    /// A radio that has not answered yet is not a radio that is off. Reporting
    /// it as off would make the page wrong every time it opened during the
    /// first moments of a launch.
    func testAnUnansweredRadioIsStartingRatherThanOff() {
        XCTAssertEqual(
            BluetoothAvailability.observed(authorizationBlocked: false, radioState: .unknown),
            .starting
        )
        XCTAssertEqual(
            BluetoothAvailability.observed(authorizationBlocked: false, radioState: .resetting),
            .starting
        )
        XCTAssertEqual(
            BluetoothAvailability.observed(authorizationBlocked: false, radioState: .poweredOn),
            .available
        )
        XCTAssertEqual(
            BluetoothAvailability.observed(authorizationBlocked: false, radioState: .poweredOff),
            .off
        )
        XCTAssertEqual(
            BluetoothAvailability.observed(authorizationBlocked: false, radioState: .unsupported),
            .off
        )
        // Denied permission is off however healthy the radio is.
        XCTAssertEqual(
            BluetoothAvailability.observed(authorizationBlocked: true, radioState: .poweredOn),
            .off
        )
    }

    func testLocalWiFiFollowsTheListeningSocketNotTheService() {
        XCTAssertEqual(
            ConnectionInputs.localWifi(.meshing(nearby: 0), listening: true),
            CoreDirectPathState.available
        )
        XCTAssertEqual(
            ConnectionInputs.localWifi(.meshing(nearby: 0), listening: false),
            CoreDirectPathState.off
        )
        XCTAssertEqual(
            ConnectionInputs.localWifi(.starting, listening: true),
            CoreDirectPathState.starting
        )
        XCTAssertEqual(
            ConnectionInputs.localWifi(.stopped, listening: true),
            CoreDirectPathState.off
        )
    }

    func testEveryRelayHealthHasExactlyOnePathState() {
        let cases: [(RelayHealth, CoreRelayPathState)] = [
            (.noConfig, .checking),
            (.checking, .checking),
            (.noInternet, .waitingForInternet),
            (.ok(lastSyncMs: Self.fixedNowMs), .connected),
            (.failing(lastAttemptMs: Self.fixedNowMs), .unreachable),
            (.expired(lastAttemptMs: Self.fixedNowMs), .passExpired),
            (.suspended(lastAttemptMs: Self.fixedNowMs), .passSuspended),
            (.tokenRejected(lastAttemptMs: Self.fixedNowMs), .setupRejected),
            (.quotaFull(lastAttemptMs: Self.fixedNowMs), .storageFull),
            (.rateLimited(lastAttemptMs: Self.fixedNowMs), .syncingSlowed),
        ]
        for (health, expected) in cases {
            XCTAssertEqual(ConnectionInputs.relay(health, configured: true), expected)
        }
    }

    /// An oversized envelope is a fact about one message, not a broken pass:
    /// the service is reachable and everything else is still moving.
    func testAnOversizedMessageIsNotABrokenPass() {
        XCTAssertEqual(
            ConnectionInputs.relay(.messageTooLarge(lastAttemptMs: Self.fixedNowMs), configured: true),
            CoreRelayPathState.connected
        )
    }

    func testNoSavedPassIsNotSetUpWhateverTheLastHealthSaid() {
        XCTAssertEqual(
            ConnectionInputs.relay(.ok(lastSyncMs: Self.fixedNowMs), configured: false),
            CoreRelayPathState.notSetUp
        )
    }

    func testOnlyTheNoInternetVerdictMeansNoValidatedInternet() {
        XCTAssertFalse(ConnectionInputs.validatedInternet(.noInternet))
        XCTAssertTrue(ConnectionInputs.validatedInternet(.ok(lastSyncMs: Self.fixedNowMs)))
        XCTAssertTrue(ConnectionInputs.validatedInternet(.failing(lastAttemptMs: Self.fixedNowMs)))
    }

    func testOnlyASuccessfulPassCarriesALastSyncTime() {
        XCTAssertEqual(ConnectionInputs.relayLastSyncMs(.ok(lastSyncMs: 1_234)), 1_234)
        XCTAssertEqual(ConnectionInputs.relayLastSyncMs(.failing(lastAttemptMs: 1_234)), 0)
    }

    func testDirectLinksMapToTheirRadio() {
        XCTAssertEqual(ConnectionInputs.directLink(.bluetooth), CoreDirectLink.bluetooth)
        XCTAssertEqual(ConnectionInputs.directLink(.localWifi), CoreDirectLink.localWifi)
        XCTAssertNil(ConnectionInputs.directLink(nil))
    }

    // MARK: - Checking bound

    func testAPendingCheckKeepsItsOriginalStartMark() {
        let clock = CheckingClock()
        XCTAssertEqual(clock.mark(pending: true, nowMs: 1_000), 1_000)
        XCTAssertEqual(clock.mark(pending: true, nowMs: 9_000), 1_000)
        XCTAssertEqual(clock.mark(pending: false, nowMs: 9_500), 0)
        XCTAssertEqual(clock.mark(pending: true, nowMs: 10_000), 10_000)
    }

    func testEveryPathThatCanStillBeComingUpCountsAsPending() {
        // Including the two radios. This platform reports CoreBluetooth
        // `.unknown` on a cold launch and `.resetting` on a radio toggle, both
        // while the mesh is already meshing -- a predicate that watched only
        // the runtime and the pass never started the bound for either, and the
        // card rendered "Needs attention" while the radio was still answering.
        XCTAssertTrue(
            connectionCheckPending(
                runtime: .starting,
                bluetooth: .available,
                localWifi: .available,
                relay: .connected
            )
        )
        XCTAssertTrue(
            connectionCheckPending(
                runtime: .active,
                bluetooth: .starting,
                localWifi: .off,
                relay: .notSetUp
            )
        )
        XCTAssertTrue(
            connectionCheckPending(
                runtime: .active,
                bluetooth: .off,
                localWifi: .starting,
                relay: .notSetUp
            )
        )
        XCTAssertTrue(
            connectionCheckPending(
                runtime: .active,
                bluetooth: .available,
                localWifi: .available,
                relay: .checking
            )
        )
        XCTAssertFalse(
            connectionCheckPending(
                runtime: .active,
                bluetooth: .available,
                localWifi: .available,
                relay: .connected
            )
        )
        XCTAssertFalse(
            connectionCheckPending(
                runtime: .stopped,
                bluetooth: .off,
                localWifi: .off,
                relay: .notSetUp
            )
        )
    }

    /// The trace the narrower predicate produced: a relaunch with the
    /// Bluetooth stack unanswered, no LAN, and no pass showed a red failure
    /// card for the few hundred milliseconds before the radio replied.
    func testARadioStillComingUpIsCheckingNotAFailure() {
        let pending = connectionCheckPending(
            runtime: ConnectionInputs.runtime(.meshing(nearby: 0), bluetooth: .starting),
            bluetooth: ConnectionInputs.bluetooth(.meshing(nearby: 0), availability: .starting),
            localWifi: ConnectionInputs.localWifi(.meshing(nearby: 0), listening: false),
            relay: ConnectionInputs.relay(.noConfig, configured: false)
        )
        XCTAssertTrue(pending)
        let result = state(
            people: [],
            relayHealth: .noConfig,
            relayConfigured: false,
            lanListening: false,
            bluetoothAvailability: .starting,
            checkingSinceMs: Self.fixedNowMs
        )
        XCTAssertEqual(result.health.state, CoreConnectionHealth.checking)
    }

    // MARK: - Freshness and event times

    func testFreshnessBuckets() {
        XCTAssertEqual(ConnectionTimes.freshness(updatedAtMs: 0, nowMs: now), .never)
        XCTAssertEqual(ConnectionTimes.freshness(updatedAtMs: now - 5_000, nowMs: now), .justNow)
        XCTAssertEqual(
            ConnectionTimes.freshness(updatedAtMs: now - 3 * minute, nowMs: now),
            .minutes(3)
        )
        XCTAssertEqual(
            ConnectionTimes.freshness(updatedAtMs: now - 2 * hour, nowMs: now),
            .hours(2)
        )
    }

    /// A snapshot stamped in the future is a clock artifact, not a reason to
    /// render a negative age.
    func testASnapshotFromTheFutureReadsAsJustNow() {
        XCTAssertEqual(
            ConnectionTimes.freshness(updatedAtMs: now + 5 * minute, nowMs: now),
            .justNow
        )
    }

    func testAZeroOrNegativeTimestampIsNeverADate() {
        let startOfToday = now - 6 * hour
        XCTAssertEqual(
            ConnectionTimes.eventTime(atMs: 0, nowMs: now, startOfTodayMs: startOfToday),
            .unknown
        )
        XCTAssertEqual(
            ConnectionTimes.eventTime(atMs: -1, nowMs: now, startOfTodayMs: startOfToday),
            .unknown
        )
    }

    func testEventTimeBuckets() {
        let startOfToday = now - 6 * hour
        XCTAssertEqual(
            ConnectionTimes.eventTime(atMs: now - 10_000, nowMs: now, startOfTodayMs: startOfToday),
            .justNow
        )
        XCTAssertEqual(
            ConnectionTimes.eventTime(
                atMs: now - 12 * minute,
                nowMs: now,
                startOfTodayMs: startOfToday
            ),
            .minutes(12)
        )
        XCTAssertEqual(
            ConnectionTimes.eventTime(
                atMs: now - 3 * hour,
                nowMs: now,
                startOfTodayMs: startOfToday
            ),
            .hours(3)
        )
        // Before today's midnight but within the day before it: yesterday, even
        // though it is under 24 hours old.
        XCTAssertEqual(
            ConnectionTimes.eventTime(
                atMs: startOfToday - hour,
                nowMs: now,
                startOfTodayMs: startOfToday
            ),
            .yesterday
        )
        XCTAssertEqual(
            ConnectionTimes.eventTime(
                atMs: startOfToday - 3 * 24 * hour,
                nowMs: now,
                startOfTodayMs: startOfToday
            ),
            .older
        )
    }

    // MARK: - Refresh coalescing

    func testABurstOfSignalsInsideTheWindowCostsExactlyOneReload() {
        let coalescer = StoreChangeCoalescer(windowMs: 500)
        XCTAssertTrue(coalescer.onSignal(nowMs: 0))
        XCTAssertFalse(coalescer.onSignal(nowMs: 10))
        XCTAssertFalse(coalescer.onSignal(nowMs: 100))
        XCTAssertEqual(coalescer.remainingMs(nowMs: 100), 400)
        XCTAssertEqual(coalescer.remainingMs(nowMs: 500), 0)
        coalescer.onReloadStarted()
        XCTAssertFalse(coalescer.onReloadFinished())
    }

    func testASignalArrivingMidReloadSchedulesExactlyOneFollowUp() {
        let coalescer = StoreChangeCoalescer(windowMs: 500)
        XCTAssertTrue(coalescer.onSignal(nowMs: 0))
        coalescer.onReloadStarted()
        XCTAssertFalse(coalescer.onSignal(nowMs: 100))
        XCTAssertFalse(coalescer.onSignal(nowMs: 200))
        XCTAssertTrue(coalescer.onReloadFinished())
        // The follow-up is owed once, not once per signal.
        coalescer.onReloadStarted()
        XCTAssertFalse(coalescer.onReloadFinished())
    }

    /// Signals that arrived while the window was open are already covered by
    /// the load that is about to run, so they owe nothing afterwards.
    func testSignalsDuringTheWaitDoNotExtendTheWindow() {
        let coalescer = StoreChangeCoalescer(windowMs: 500)
        XCTAssertTrue(coalescer.onSignal(nowMs: 0))
        XCTAssertFalse(coalescer.onSignal(nowMs: 400))
        XCTAssertEqual(coalescer.remainingMs(nowMs: 400), 100)
        coalescer.onReloadStarted()
        XCTAssertFalse(coalescer.onReloadFinished())
    }

    func testABackwardsClockCannotStallThePageBehindAHugeWait() {
        let coalescer = StoreChangeCoalescer(windowMs: 500)
        XCTAssertTrue(coalescer.onSignal(nowMs: 10_000_000))
        XCTAssertEqual(coalescer.remainingMs(nowMs: 0), 500)
    }

    func testNothingIsOwedBeforeAnythingIsSignalled() {
        let coalescer = StoreChangeCoalescer(windowMs: 500)
        XCTAssertEqual(coalescer.remainingMs(nowMs: 0), 0)
        XCTAssertFalse(coalescer.onReloadFinished())
    }

    /// The page is torn down every time it goes away, and the loop can be
    /// cancelled inside the window or inside the load. Without the reset the
    /// coalescer spends the rest of its life absorbing every signal as "a
    /// reload is already running", and the page never loads again.
    func testResetForgetsAWindowOrAReloadLeftOutstandingByATeardown() {
        let coalescer = StoreChangeCoalescer(windowMs: 500)
        // Cancelled inside the window.
        XCTAssertTrue(coalescer.onSignal(nowMs: 0))
        XCTAssertFalse(coalescer.onSignal(nowMs: 0))
        coalescer.reset()
        XCTAssertTrue(coalescer.onSignal(nowMs: 1_000))

        // Cancelled inside the load.
        coalescer.onReloadStarted()
        XCTAssertFalse(coalescer.onSignal(nowMs: 1_000))
        coalescer.reset()
        XCTAssertTrue(coalescer.onSignal(nowMs: 2_000))
        // And nothing is owed from before the teardown.
        coalescer.onReloadStarted()
        XCTAssertFalse(coalescer.onReloadFinished())
    }

    // MARK: - Delivery language

    private func facts(
        waitingCount: Int,
        oldestWaitingMs: Int64,
        lastProgressMs: Int64,
        oversizedWaiting: Bool = false,
        relayRejectStreak: Int64 = 0,
        /// By default nothing has been handed over yet.
        unpostedWaitingCount: Int? = nil
    ) -> PersonDeliveryFacts {
        PersonDeliveryFacts(
            waitingCount: waitingCount,
            unpostedWaitingCount: unpostedWaitingCount ?? waitingCount,
            oldestWaitingMs: oldestWaitingMs,
            lastProgressMs: lastProgressMs,
            oversizedWaiting: oversizedWaiting,
            relayRejectStreak: relayRejectStreak,
            relayRejectedAtMs: relayRejectStreak > 0 ? Self.fixedNowMs : 0,
            relayUnreachableStreak: 0,
            relayUnreachableAtMs: 0
        )
    }

    private func deliveryLine(
        waitingCount: Int,
        directLink: Bool = false,
        ownRelayUsable: Bool = true,
        hasRelayEndpoint: Bool = true,
        ageMs: Int64 = ConnectionDetailsLogicTests.oneMinuteMs,
        oversizedWaiting: Bool = false,
        relayRejectStreak: Int64 = 0,
        relay: CoreRelayPathState = .connected
    ) -> CoreDeliveryLine? {
        DeliveryPresentation.line(
            person: person(
                1,
                "Ash",
                hasRelayEndpoint: hasRelayEndpoint,
                delivery: facts(
                    waitingCount: waitingCount,
                    oldestWaitingMs: Self.fixedNowMs - ageMs,
                    lastProgressMs: Self.fixedNowMs - ageMs,
                    oversizedWaiting: oversizedWaiting,
                    relayRejectStreak: relayRejectStreak
                )
            ),
            directLink: directLink,
            ownRelayUsable: ownRelayUsable,
            relay: relay,
            nowMs: Self.fixedNowMs
        )
    }

    func testNothingWaitingMeansNoDeliveryLineAtAll() {
        XCTAssertNil(deliveryLine(waitingCount: 0))
    }

    func testALiveLinkMeansTheWorkIsGoingOutNow() {
        let line = deliveryLine(waitingCount: 2, directLink: true, ownRelayUsable: false)
        XCTAssertEqual(line?.state, CoreDeliveryState.sending)
        XCTAssertEqual(line?.count, 2)
    }

    func testAWorkingPassPlusTheirEndpointIsAlsoAUsableRoute() {
        XCTAssertEqual(deliveryLine(waitingCount: 1)?.state, CoreDeliveryState.sending)
    }

    func testNoInternetWithOnlyAPassRouteSaysSoPlainly() {
        let line = deliveryLine(
            waitingCount: 3,
            ownRelayUsable: false,
            relay: .waitingForInternet
        )
        XCTAssertEqual(line?.state, CoreDeliveryState.waitingForInternet)
        XCTAssertEqual(line?.count, 3)
    }

    /// The movement state stays a promise underneath the fault. An expired pass
    /// stops the internet route; it does not stop the next encounter, and the
    /// copy must not say otherwise.
    func testAPassFaultStillPromisesTheNextEncounterRatherThanAFailure() {
        let line = deliveryLine(waitingCount: 4, ownRelayUsable: false, relay: .passExpired)
        XCTAssertEqual(line?.state, CoreDeliveryState.willDeliverWhenReconnected)
        XCTAssertEqual(line?.blockedReason, CoreDeliveryBlockedReason.passExpired)
    }

    /// The DTN invariant, asserted at an age where a naive threshold would have
    /// fired a thousand times over.
    func testAFriendWhoIsMerelyOfflineIsNeverAnErrorAtAnyAge() {
        let line = DeliveryPresentation.line(
            person: person(
                1,
                "Ash",
                hasRelayEndpoint: false,
                delivery: facts(
                    waitingCount: 6,
                    oldestWaitingMs: Self.fixedNowMs - 10 * 24 * ConnectionDetailsLogicTests.oneHourMs,
                    lastProgressMs: 0
                )
            ),
            directLink: false,
            ownRelayUsable: false,
            relay: .connected,
            nowMs: Self.fixedNowMs
        )
        XCTAssertEqual(line?.state, CoreDeliveryState.willDeliverWhenReconnected)
        XCTAssertEqual(line?.delayed, false)
        XCTAssertNil(line?.blockedReason)
        XCTAssertNil(line?.attention)
    }

    func testAUsableRouteThatHasCarriedNothingForTheWindowReadsAsDelayed() {
        let line = deliveryLine(waitingCount: 2, ageMs: 30 * ConnectionDetailsLogicTests.oneMinuteMs)
        XCTAssertEqual(line?.delayed, true)
        XCTAssertEqual(line?.attention, CorePersonAttention.delayed)
        // Still sending underneath: the path works, it is just not moving.
        XCTAssertEqual(line?.state, CoreDeliveryState.sending)
    }

    /// Our pass works, their endpoint is healthy, every message was accepted --
    /// and their phone is off. A successful upload is the last progress this
    /// device can record, so an age-only rule would park this friend in Needs
    /// attention overnight, every night, with nothing to do about it.
    func testAFriendWhoHasNotCollectedMailWeAlreadySentIsNeverDelayed() {
        let threeDaysMs = 3 * 24 * ConnectionDetailsLogicTests.oneHourMs
        let line = DeliveryPresentation.line(
            person: person(
                1,
                "Ash",
                hasRelayEndpoint: true,
                delivery: facts(
                    waitingCount: 2,
                    oldestWaitingMs: Self.fixedNowMs - threeDaysMs,
                    lastProgressMs: Self.fixedNowMs - threeDaysMs,
                    unpostedWaitingCount: 0
                )
            ),
            directLink: false,
            ownRelayUsable: true,
            relay: .connected,
            nowMs: Self.fixedNowMs
        )
        XCTAssertEqual(line?.state, CoreDeliveryState.sending)
        XCTAssertEqual(line?.delayed, false)
        XCTAssertNil(line?.attention)
    }

    func testTheirRejectedCardIsTheMostSevereAttentionThereIs() {
        let line = deliveryLine(waitingCount: 5, relayRejectStreak: 4)
        XCTAssertEqual(line?.blockedReason, CoreDeliveryBlockedReason.contactSetupRejected)
        XCTAssertEqual(line?.attention, CorePersonAttention.setupRejected)
    }

    func testAnOversizedMessageIsTerminalEvenWithTheFriendInTheRoom() {
        let line = deliveryLine(waitingCount: 1, directLink: true, oversizedWaiting: true)
        XCTAssertEqual(line?.blockedReason, CoreDeliveryBlockedReason.messageTooLarge)
        XCTAssertEqual(line?.attention, CorePersonAttention.messageTooLarge)
    }

    /// The "red under every friend" failure, in one assertion: a friend whose
    /// card carries no endpoint is untouched by our expired pass.
    func testOurOwnPassFaultNeverReachesAFriendTheInternetWasNotARouteTo() {
        let line = deliveryLine(
            waitingCount: 3,
            ownRelayUsable: false,
            hasRelayEndpoint: false,
            relay: .passExpired
        )
        XCTAssertNil(line?.blockedReason)
        XCTAssertNil(line?.attention)
    }

    // MARK: - Best route

    private func bestRoute(
        directLink: CoreDirectLink? = nil,
        ownRelayUsable: Bool = true,
        hasRelayEndpoint: Bool = true,
        relayRejectStreak: Int64 = 0
    ) -> CorePersonRoute {
        DeliveryPresentation.bestRoute(
            person: person(
                1,
                "Ash",
                hasRelayEndpoint: hasRelayEndpoint,
                delivery: facts(
                    waitingCount: 0,
                    oldestWaitingMs: 0,
                    lastProgressMs: 0,
                    relayRejectStreak: relayRejectStreak
                )
            ),
            directLink: directLink,
            ownRelayUsable: ownRelayUsable,
            nowMs: Self.fixedNowMs
        )
    }

    /// A post-only friend card must not read as broken, which is why this is
    /// the core's answer and not a second derivation on this side of the FFI.
    func testTheBestRouteRestatesTheCoreAnswerRatherThanReDerivingIt() {
        XCTAssertEqual(bestRoute(directLink: .bluetooth), CorePersonRoute.directBluetooth)
        XCTAssertEqual(bestRoute(directLink: .localWifi), CorePersonRoute.directLocalWifi)
        XCTAssertEqual(bestRoute(), CorePersonRoute.shorePass)
        XCTAssertEqual(bestRoute(ownRelayUsable: false), CorePersonRoute.noneNow)
        XCTAssertEqual(bestRoute(hasRelayEndpoint: false), CorePersonRoute.noneNow)
        XCTAssertEqual(bestRoute(relayRejectStreak: 4), CorePersonRoute.noneNow)
    }

    // MARK: - Waiting age

    func testAWaitingAgeIsADurationAndNeverADate() {
        XCTAssertEqual(ConnectionTimes.waitingAge(sinceMs: 0, nowMs: now), .unknown)
        // A stamp from the future is a clock artifact, not a negative age.
        XCTAssertEqual(ConnectionTimes.waitingAge(sinceMs: now + minute, nowMs: now), .unknown)
        // Under a minute renders nothing rather than "0 min".
        XCTAssertEqual(ConnectionTimes.waitingAge(sinceMs: now - 30_000, nowMs: now), .unknown)
        XCTAssertEqual(
            ConnectionTimes.waitingAge(sinceMs: now - 14 * minute, nowMs: now),
            .minutes(14)
        )
        XCTAssertEqual(
            ConnectionTimes.waitingAge(sinceMs: now - 3 * hour, nowMs: now),
            .hours(3)
        )
        XCTAssertEqual(
            ConnectionTimes.waitingAge(sinceMs: now - 48 * hour, nowMs: now),
            .days(2)
        )
    }

    // MARK: - View-state assembly

    private func userId(_ id: UInt8) -> Data { Data([id]) }

    private func person(
        _ id: UInt8,
        _ name: String,
        blocked: Bool = false,
        hasRelayEndpoint: Bool = true,
        delivery: PersonDeliveryFacts = PersonDeliveryFacts.none,
        latest: PersonEvidence? = nil,
        lastDeliveredMs: Int64 = 0
    ) -> ConnectionPerson {
        let bytes = userId(id)
        return ConnectionPerson(
            userId: bytes,
            userIdHex: UserIdHex.encode(bytes),
            name: name,
            blocked: blocked,
            hasRelayEndpoint: hasRelayEndpoint,
            delivery: delivery,
            latest: latest,
            lastDeliveredMs: lastDeliveredMs
        )
    }

    /// `count` messages that started waiting `ageMs` ago and have not moved since.
    private func waiting(
        _ count: Int,
        ageMs: Int64 = ConnectionDetailsLogicTests.oneMinuteMs
    ) -> PersonDeliveryFacts {
        facts(
            waitingCount: count,
            oldestWaitingMs: Self.fixedNowMs - ageMs,
            lastProgressMs: Self.fixedNowMs - ageMs
        )
    }

    private func state(
        people: [ConnectionPerson],
        directPaths: [Data: DirectPath] = [:],
        relayHealth: RelayHealth = .ok(lastSyncMs: ConnectionDetailsLogicTests.fixedNowMs),
        relayConfigured: Bool = true,
        lanListening: Bool = true,
        runtimeState: MeshRuntimeState = .meshing(nearby: 0),
        bluetoothAvailability: BluetoothAvailability = .available,
        /// Zero, unless a test is about the bounded Checking state: a mark of
        /// zero is "nothing pending" and resolves the bound immediately.
        checkingSinceMs: Int64 = 0,
        presence: [Data: Int64] = [:],
        activity: [ConnectionActivityRow] = []
    ) -> ConnectionDetailsState {
        ConnectionDetailsLogic.buildState(
            runtimeState: runtimeState,
            bluetoothAvailability: bluetoothAvailability,
            directPaths: directPaths,
            relayHealth: relayHealth,
            relayConfigured: relayConfigured,
            lanListening: lanListening,
            bluetoothAudioActive: false,
            presenceLastSeen: presence,
            contactLastSeen: [:],
            snapshot: ConnectionStoreSnapshot(
                people: people,
                activity: activity,
                loadedAtMs: Self.fixedNowMs
            ),
            checkingSinceMs: checkingSinceMs,
            refreshing: false,
            nowMs: Self.fixedNowMs
        )
    }

    func testAQuietPhoneWithNobodyNearbyIsWorkingNormally() {
        let result = state(people: [person(1, "Ash")])
        XCTAssertEqual(result.health.state, CoreConnectionHealth.ready)
        XCTAssertNil(result.health.reason)
        XCTAssertNil(result.health.action)
        XCTAssertEqual(result.health.nearbyFriendCount, 0)
        XCTAssertTrue(result.needsAttention.isEmpty)
        XCTAssertTrue(result.reachableNow.isEmpty)
        XCTAssertEqual(result.otherPeople.map { $0.name }, ["Ash"])
    }

    func testALiveLinkPutsTheFriendInReachableNowWithTheRightBadge() {
        let result = state(
            people: [person(1, "Riley's phone"), person(2, "Sam")],
            directPaths: [userId(1): .localWifi, userId(2): .bluetooth]
        )
        XCTAssertEqual(result.reachableNow.map { $0.name }, ["Riley's phone", "Sam"])
        XCTAssertEqual(result.reachableNow[0].badge, .localWifi)
        XCTAssertEqual(result.reachableNow[1].badge, .bluetooth)
        XCTAssertEqual(result.reachableNow[0].status, .connectedNow)
        XCTAssertEqual(result.health.nearbyFriendCount, 2)
        XCTAssertEqual(result.paths.localWifiLinks, 1)
        XCTAssertEqual(result.paths.bluetoothLinks, 1)
    }

    /// A stranger's phone passing by is not someone this page can promise
    /// anything about, so it is not a friend nearby.
    func testAStrangerNearbyIsNotAFriendNearby() {
        let result = state(
            people: [person(1, "Ash")],
            directPaths: [userId(9): .bluetooth]
        )
        XCTAssertEqual(result.health.nearbyFriendCount, 0)
        XCTAssertEqual(result.paths.bluetoothLinks, 0)
        XCTAssertTrue(result.reachableNow.isEmpty)
    }

    func testFreshPresenceWithAWorkingPassReadsAsSeenOnline() {
        let seenAt = Self.fixedNowMs - minute
        let result = state(
            people: [person(1, "Ash")],
            presence: [userId(1): seenAt]
        )
        XCTAssertEqual(result.reachableNow.map { $0.name }, ["Ash"])
        XCTAssertEqual(result.reachableNow[0].badge, .shorePass)
        XCTAssertEqual(result.reachableNow[0].status, .seenOnline(atMs: seenAt))
    }

    /// The old page's flagship contradiction, in both places it could show.
    func testShorePassConnectedWithoutInternetNeverClaimsToBeConnected() {
        let result = state(
            people: [person(1, "Ash")],
            relayHealth: .noInternet,
            presence: [userId(1): Self.fixedNowMs - minute]
        )
        XCTAssertEqual(result.health.relay, CoreRelayPathState.waitingForInternet)
        XCTAssertEqual(result.paths.relay, CoreRelayPathState.waitingForInternet)
        // And a friend seen a minute ago is not promised over a path this
        // phone does not have.
        XCTAssertTrue(result.reachableNow.isEmpty)
        XCTAssertEqual(result.otherPeople.map { $0.name }, ["Ash"])
    }

    func testABlockedFriendAppearsInNoGroup() {
        let result = state(
            people: [person(9, "Blocked", blocked: true), person(2, "Sam")],
            directPaths: [userId(9): .bluetooth]
        )
        let allNames = (result.needsAttention + result.reachableNow + result.otherPeople)
            .map { $0.name }
        XCTAssertEqual(allNames, ["Sam"])
    }

    func testAFriendWithNoHistorySaysSoInsteadOfInventingADate() {
        let result = state(people: [person(1, "Dana")])
        XCTAssertEqual(result.otherPeople[0].status, .noHistory)
        XCTAssertNil(result.otherPeople[0].badge)
    }

    func testTheNewestEvidenceBecomesTheStatusSentenceAndItsBadge() {
        let at = Self.fixedNowMs - 2 * minute
        let result = state(
            people: [
                person(
                    1,
                    "Ash",
                    latest: PersonEvidence(evidence: .messageDelivered, path: .shorePass, atMs: at)
                )
            ]
        )
        XCTAssertEqual(
            result.otherPeople[0].status,
            .history(evidence: .messageDelivered, atMs: at)
        )
        XCTAssertEqual(result.otherPeople[0].badge, .shorePass)
    }

    /// A carried arrival names no radio: another phone brought it the last hop.
    func testACarriedArrivalKeepsNoBadge() {
        let result = state(
            people: [
                person(
                    1,
                    "Ash",
                    latest: PersonEvidence(
                        evidence: .messageReceived,
                        path: nil,
                        atMs: Self.fixedNowMs - minute
                    )
                )
            ]
        )
        XCTAssertNil(result.otherPeople[0].badge)
    }

    func testWaitingWorkNeverRendersAsAnErrorWhateverThePathState() {
        let result = state(
            people: [person(1, "Ash", delivery: waiting(3))],
            relayHealth: .noInternet
        )
        let delivery = result.otherPeople[0].delivery
        XCTAssertEqual(delivery?.state, CoreDeliveryState.waitingForInternet)
        XCTAssertEqual(delivery?.count, 3)
        XCTAssertNil(delivery?.blockedReason)
        // Not in Needs attention either: no fault, no stall, nothing to do.
        XCTAssertTrue(result.needsAttention.isEmpty)
    }

    /// The contradiction this page exists to remove. The row says "Received
    /// your message 12 min ago"; the old page put "Sending 12 messages…"
    /// directly beneath it for as long as the retention window lasted. Phase 2
    /// makes that impossible upstream: the count is receipt-aware, so a
    /// satisfied conversation arrives here as zero.
    func testAFriendWhoAlreadyReceivedAMessageGetsNoQueueLineUnderTheRow() {
        let receivedAt = Self.fixedNowMs - 12 * minute
        let result = state(
            people: [
                person(
                    1,
                    "Ash",
                    latest: PersonEvidence(
                        evidence: .messageDelivered,
                        path: .shorePass,
                        atMs: receivedAt
                    ),
                    lastDeliveredMs: receivedAt
                )
            ]
        )
        XCTAssertEqual(
            result.otherPeople[0].status,
            .history(evidence: .messageDelivered, atMs: receivedAt)
        )
        XCTAssertNil(result.otherPeople[0].delivery)
        XCTAssertEqual(result.otherPeople[0].detail.lastDeliveredMs, receivedAt)
    }

    /// Grouped by the same verdict their row renders -- one classification,
    /// used twice, so a row cannot be filed under a problem it never states.
    func testAFriendNeedingAttentionLeadsThePageAndStatesWhyInTheirOwnRow() {
        let waitingSince = Self.fixedNowMs - 14 * minute
        let result = state(
            people: [
                person(1, "Bo"),
                person(
                    2,
                    "Ash",
                    delivery: facts(
                        waitingCount: 2,
                        oldestWaitingMs: waitingSince,
                        lastProgressMs: waitingSince,
                        relayRejectStreak: 4
                    )
                ),
            ]
        )
        XCTAssertEqual(result.needsAttention.map { $0.name }, ["Ash"])
        let row = result.needsAttention[0]
        XCTAssertEqual(row.attention, CorePersonAttention.setupRejected)
        XCTAssertEqual(
            row.delivery?.blockedReason,
            CoreDeliveryBlockedReason.contactSetupRejected
        )
        XCTAssertEqual(row.delivery?.oldestWaitingMs, waitingSince)
        // And nowhere else on the page.
        XCTAssertEqual(result.otherPeople.map { $0.name }, ["Bo"])
        XCTAssertTrue(result.reachableNow.isEmpty)
    }

    func testADelayedFriendNeedsAttentionWithoutTheirRowBecomingAnError() {
        let result = state(people: [person(1, "Ash", delivery: waiting(2, ageMs: 30 * minute))])
        XCTAssertEqual(result.needsAttention.count, 1)
        let row = result.needsAttention[0]
        XCTAssertEqual(row.attention, CorePersonAttention.delayed)
        XCTAssertEqual(row.delivery?.delayed, true)
        XCTAssertNil(row.delivery?.blockedReason)
    }

    func testThePersonDetailCarriesTheCoreRouteAndTheTimesItCanProve() {
        let deliveredAt = Self.fixedNowMs - 5 * minute
        let result = state(
            people: [person(1, "Sam", lastDeliveredMs: deliveredAt)],
            directPaths: [userId(1): .bluetooth]
        )
        let detail = result.reachableNow[0].detail
        XCTAssertEqual(detail.bestRoute, CorePersonRoute.directBluetooth)
        XCTAssertEqual(detail.lastDeliveredMs, deliveredAt)
    }

    /// A block is a tombstone. The most eye-catching group on the page is
    /// exactly where a leak would be most visible.
    func testABlockedFriendWithARejectedCardIsNotPromotedIntoNeedsAttention() {
        let result = state(
            people: [
                person(
                    1,
                    "Blocked",
                    blocked: true,
                    delivery: facts(
                        waitingCount: 9,
                        oldestWaitingMs: Self.fixedNowMs - hour,
                        lastProgressMs: Self.fixedNowMs - hour,
                        relayRejectStreak: 4
                    )
                )
            ]
        )
        XCTAssertTrue(result.needsAttention.isEmpty)
        XCTAssertTrue(result.reachableNow.isEmpty)
        XCTAssertTrue(result.otherPeople.isEmpty)
        XCTAssertFalse(result.hasContacts)
    }

    /// Not in a group, and not in the numbers above the groups either: a count
    /// only a blocked person produces discloses them just as surely.
    func testABlockedFriendStandingNextToUsIsCountedNowhere() {
        let result = state(
            people: [person(1, "Ash", blocked: true), person(2, "Bo")],
            directPaths: [userId(1): .bluetooth]
        )
        XCTAssertTrue(result.reachableNow.isEmpty)
        XCTAssertEqual(result.otherPeople.map { $0.name }, ["Bo"])
        XCTAssertEqual(result.health.nearbyFriendCount, 0)
        XCTAssertEqual(result.paths.bluetoothLinks, 0)
    }

    func testAStoppedMeshNeedsAttentionAndOffersToStart() {
        let result = state(people: [person(1, "Ash")], runtimeState: .stopped)
        XCTAssertEqual(result.health.state, CoreConnectionHealth.needsAttention)
        XCTAssertEqual(result.health.reason, CoreHealthReason.meshStopped)
        XCTAssertEqual(result.health.action, CoreHealthAction.startMesh)
    }

    /// One radio down while another path still carries messages is a limit,
    /// not a breakage.
    func testBluetoothOffWithAWorkingPassIsLimitedNotBroken() {
        let result = state(
            people: [person(1, "Ash")],
            lanListening: false,
            bluetoothAvailability: .off
        )
        XCTAssertEqual(result.health.state, CoreConnectionHealth.limited)
        XCTAssertEqual(result.health.bluetooth, CoreDirectPathState.off)
    }

    func testNoSavedPassIsStillWorkingNormally() {
        let result = state(
            people: [person(1, "Ash")],
            relayHealth: .noConfig,
            relayConfigured: false
        )
        XCTAssertEqual(result.health.state, CoreConnectionHealth.ready)
        XCTAssertEqual(result.health.relay, CoreRelayPathState.notSetUp)
    }

    func testTheLastSuccessfulSyncTimeComesThroughForThePathsRow() {
        let syncedAt = Self.fixedNowMs - 4 * minute
        let result = state(
            people: [person(1, "Ash")],
            relayHealth: .ok(lastSyncMs: syncedAt)
        )
        XCTAssertEqual(result.paths.relayLastSyncMs, syncedAt)
    }

    func testAnEmptyAddressBookIsReportedAsEmpty() {
        let result = state(people: [])
        XCTAssertFalse(result.hasContacts)
        XCTAssertTrue(result.reachableNow.isEmpty)
        XCTAssertTrue(result.otherPeople.isEmpty)
    }

    func testAPageWithOnlyBlockedFriendsHasNoContactsToShow() {
        let result = state(people: [person(9, "Blocked", blocked: true)])
        XCTAssertFalse(result.hasContacts)
    }
}
