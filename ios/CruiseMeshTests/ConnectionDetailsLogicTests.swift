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

    private func deliveryLine(
        queued: Int,
        routeIsDirect: Bool = false,
        ownRelayUsable: Bool = true,
        contactHasRelayEndpoint: Bool = true,
        contactRelayStale: Bool = false,
        relay: CoreRelayPathState = .connected,
        receiptIsNewestEvidence: Bool = false
    ) -> DeliveryLine? {
        DeliveryPresentation.line(
            queued: queued,
            routeIsDirect: routeIsDirect,
            ownRelayUsable: ownRelayUsable,
            contactHasRelayEndpoint: contactHasRelayEndpoint,
            contactRelayStale: contactRelayStale,
            relay: relay,
            receiptIsNewestEvidence: receiptIsNewestEvidence
        )
    }

    func testNothingWaitingMeansNoDeliveryLineAtAll() {
        XCTAssertNil(deliveryLine(queued: 0))
    }

    func testALiveLinkMeansTheWorkIsGoingOutNow() {
        XCTAssertEqual(
            deliveryLine(queued: 2, routeIsDirect: true, ownRelayUsable: false),
            DeliveryLine(kind: .sending, count: 2)
        )
    }

    func testAWorkingPassPlusTheirEndpointIsAlsoAUsableRoute() {
        XCTAssertEqual(deliveryLine(queued: 1), DeliveryLine(kind: .sending, count: 1))
    }

    /// Nothing will ever mark these rows uploaded, so the number is not a
    /// backlog: it is every message written to them inside the retention
    /// window, and it would sit under their name for a week.
    func testAWrittenOffEndpointMeansNoLineBecauseTheCountCannotDrain() {
        XCTAssertNil(deliveryLine(queued: 4, contactRelayStale: true))
    }

    func testNoInternetWithOnlyAPassRouteSaysSoPlainly() {
        XCTAssertEqual(
            deliveryLine(queued: 3, ownRelayUsable: false, relay: .waitingForInternet),
            DeliveryLine(kind: .waitingForInternet, count: 3)
        )
    }

    func testAPassFaultStillPromisesTheNextEncounterRatherThanAFailure() {
        XCTAssertEqual(
            deliveryLine(queued: 4, ownRelayUsable: false, relay: .passExpired),
            DeliveryLine(kind: .willDeliverWhenReconnected, count: 4)
        )
    }

    /// The contradiction this page exists to remove. The count is rows whose
    /// upload stamp is unset, and only an upload sets it -- not a receipt, and
    /// not handing the message over in person. On a phone with no pass saved,
    /// or for a friend whose card carries no endpoint, the number never goes
    /// down.
    func testABacklogThatRelayUploadCannotDrainIsNotShownAtAll() {
        XCTAssertNil(
            deliveryLine(
                queued: 12,
                routeIsDirect: true,
                ownRelayUsable: false,
                relay: .notSetUp
            )
        )
        XCTAssertNil(deliveryLine(queued: 12, contactHasRelayEndpoint: false))
    }

    func testARowThatAlreadySaysTheyReceivedAMessageGetsNoLineUnderIt() {
        XCTAssertNil(deliveryLine(queued: 12, receiptIsNewestEvidence: true))
    }

    // MARK: - View-state assembly

    private func userId(_ id: UInt8) -> Data { Data([id]) }

    private func person(
        _ id: UInt8,
        _ name: String,
        blocked: Bool = false,
        hasRelayEndpoint: Bool = true,
        queued: Int = 0,
        latest: PersonEvidence? = nil
    ) -> ConnectionPerson {
        let bytes = userId(id)
        return ConnectionPerson(
            userId: bytes,
            userIdHex: UserIdHex.encode(bytes),
            name: name,
            blocked: blocked,
            hasRelayEndpoint: hasRelayEndpoint,
            queued: queued,
            latest: latest
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
        stale: Set<Data> = [],
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
            staleRelayContacts: stale,
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
        let allNames = (result.reachableNow + result.otherPeople).map { $0.name }
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
            people: [person(1, "Ash", queued: 3)],
            relayHealth: .noInternet
        )
        XCTAssertEqual(
            result.otherPeople[0].delivery,
            DeliveryLine(kind: .waitingForInternet, count: 3)
        )
    }

    /// The contradiction in one assertion: the row says "Received your message
    /// 12 min ago" and the old page put "Sending 12 messages…" directly
    /// beneath it, for as long as the retention window lasted.
    func testAFriendWhoAlreadyReceivedAMessageGetsNoQueueLineUnderTheRow() {
        let receivedAt = Self.fixedNowMs - 12 * minute
        let result = state(
            people: [
                person(
                    1,
                    "Ash",
                    queued: 12,
                    latest: PersonEvidence(
                        evidence: .messageDelivered,
                        path: .shorePass,
                        atMs: receivedAt
                    )
                )
            ]
        )
        XCTAssertEqual(
            result.otherPeople[0].status,
            .history(evidence: .messageDelivered, atMs: receivedAt)
        )
        XCTAssertNil(result.otherPeople[0].delivery)
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
