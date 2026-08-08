import Foundation

/**
 Everything the Connection details page decides *before* it touches SwiftUI.

 The interpretation itself is not here -- it is in the core
 (`core/src/connection_health.rs`), so iOS and Android cannot drift apart.
 What lives here is the narrow shell-side work the core deliberately does not
 do: turning this platform's observable signals into the core's inputs,
 turning the core's answer plus the store snapshot into a flat view state,
 and the two pure UI policies the core has no opinion on -- how fresh a
 timestamp reads, and when a burst of store-change signals is allowed to
 cause a reload.

 Nothing in this file imports SwiftUI, CoreBluetooth, or Network, so all of it
 is unit tested directly. Nothing in it produces user-facing text either: the
 view state carries enums and counts, and `ConnectionDetailsView` renders them
 through `Localizable.xcstrings`, where the localization gate can see the copy.

 Mirrors `ConnectionDetailsLogic.kt` in the Android shell, member for member.
 */

// MARK: - View state

/// Which path a badge or a Paths row names.
enum ConnectionPathBadge: Equatable {
    case bluetooth
    case localWifi
    case shorePass
}

/// What this phone's Bluetooth radio can do, as the shell observed it.
///
/// `starting` is "no verdict yet" (`CBManagerState.unknown` / `.resetting`),
/// which must not be reported as a fault: the radio answers a fraction of a
/// second after launch and a page that said "Bluetooth is off" in the meantime
/// would be wrong more often than right.
enum BluetoothAvailability: Equatable {
    case available
    case off
    case starting
}

/// The status sentence under a person's name.
///
/// `noHistory` is a first-class case rather than a missing timestamp, because
/// "friend added five minutes ago, never met yet" must read as itself and never
/// as a date derived from a zero.
enum PersonStatus: Equatable {
    /// A live direct link exists right now.
    case connectedNow
    /// No live link, but their relay presence is fresh and our own pass works.
    case seenOnline(atMs: Int64)
    /// The newest recorded evidence for this person.
    case history(evidence: PeerEvidence, atMs: Int64)
    /// Nothing has ever been recorded for this person.
    case noHistory
}

/**
 The user-visible meaning of messages still waiting for this person.

 The states themselves are the core's (`CoreDeliveryState`); this record only
 carries one to the renderer with its count.
 */
struct DeliveryLine: Equatable {
    let kind: CoreDeliveryState
    let count: Int
}

struct ConnectionPersonRow: Equatable {
    let userIdHex: String
    let name: String
    let status: PersonStatus
    let badge: ConnectionPathBadge?
    let delivery: DeliveryLine?
}

struct ConnectionActivityRow: Equatable {
    /// Nil when the event belongs to an identity that is not a contact.
    let name: String?
    let evidence: PeerEvidence
    /// Nil when no path was observed, which is exactly a carried arrival.
    let path: ConnectionPathBadge?
    let atMs: Int64
}

/**
 The health card's facts. The states, reasons, and actions are the core's --
 this record only carries them to the renderer.
 */
struct HealthCardState: Equatable {
    let state: CoreConnectionHealth
    let nearbyFriendCount: Int
    let bluetooth: CoreDirectPathState
    let relay: CoreRelayPathState
    let reason: CoreHealthReason?
    let action: CoreHealthAction?
}

/**
 *This phone's* paths. A friend's endpoint problem is that friend's row and
 never a row here -- mixing the two is how the old page manufactured
 contradictions.
 */
struct PathsCardState: Equatable {
    let bluetooth: CoreDirectPathState
    let bluetoothLinks: Int
    let bluetoothAudioActive: Bool
    let localWifiLinks: Int
    let relay: CoreRelayPathState
    /// Last successful Shore Pass sync, epoch ms; `0` when there has been none.
    let relayLastSyncMs: Int64
}

struct ConnectionDetailsState {
    let health: HealthCardState
    let paths: PathsCardState
    let reachableNow: [ConnectionPersonRow]
    let otherPeople: [ConnectionPersonRow]
    let hasContacts: Bool
    let activity: [ConnectionActivityRow]
    /// Epoch ms the snapshot behind this state was loaded; `0` before the first load.
    let updatedAtMs: Int64
    let refreshing: Bool
}

// MARK: - Store snapshot (produced off the main actor, consumed here)

/**
 The newest recorded evidence for one person, with its path already reduced to
 what may be *named* on screen.

 The reduction happens at load time on purpose: a carried arrival keeps no
 nameable path, and storing the raw transport here would leave every renderer
 free to invent one.
 */
struct PersonEvidence: Equatable {
    let evidence: PeerEvidence
    let path: ConnectionPathBadge?
    let atMs: Int64
}

/// One person as the background load found them.
struct ConnectionPerson: Equatable {
    let userId: Data
    let userIdHex: String
    let name: String
    let blocked: Bool
    /// Their friend card carries an internet-delivery endpoint.
    let hasRelayEndpoint: Bool
    /// User-visible messages still waiting to go out to them.
    let queued: Int
    /// Newest recorded evidence across every path, or nil when there is none.
    let latest: PersonEvidence?
}

/**
 Everything the page needs from the store, as one finished value.

 Every member is an immutable value type declared in this module, so the whole
 snapshot crosses back from the background load without any of it being shared
 mutable state.
 */
struct ConnectionStoreSnapshot: Equatable {
    let people: [ConnectionPerson]
    let activity: [ConnectionActivityRow]
    let loadedAtMs: Int64

    static let empty = ConnectionStoreSnapshot(people: [], activity: [], loadedAtMs: 0)
}

// MARK: - Platform signals -> core inputs

/**
 Translates this platform's observable signals into the core's vocabulary.

 Every function here is a mapping, never a decision. The moment one of them
 starts deciding something, the two shells have started to drift again.
 */
enum ConnectionInputs {

    /**
     The mesh runtime.

     `syncingViaRelay` is a running service in the middle of one relay pass,
     not a separate mode, so it maps to the same `active` as `meshing`; the
     Shore Pass path state says everything there is to say about that pass.
     A running service whose Bluetooth radio is off is the core's
     `bluetoothOff` -- exactly the state a person lands in after switching
     Bluetooth off in Control Center and forgetting it.
     */
    static func runtime(
        _ state: MeshRuntimeState,
        bluetooth: BluetoothAvailability
    ) -> CoreMeshRuntime {
        switch state {
        case .stopped:
            return .stopped
        case .starting:
            return .starting
        case .meshing, .syncingViaRelay:
            return bluetooth == .off ? .bluetoothOff : .active
        }
    }

    /// Bluetooth availability, from the radio itself rather than from whether
    /// the service is nominally running.
    static func bluetooth(
        _ state: MeshRuntimeState,
        availability: BluetoothAvailability
    ) -> CoreDirectPathState {
        switch state {
        case .stopped:
            return .off
        case .starting:
            return .starting
        case .meshing, .syncingViaRelay:
            switch availability {
            case .available: return .available
            case .off: return .off
            case .starting: return .starting
            }
        }
    }

    /**
     Local Wi-Fi availability, from whether the LAN transport actually holds a
     listening socket -- not from whether the service is nominally running.

     Only the *existence* of the endpoint is read. The endpoint itself is never
     carried into the view state, let alone rendered: addresses and network
     names stay off this page.
     */
    static func localWifi(_ state: MeshRuntimeState, listening: Bool) -> CoreDirectPathState {
        switch state {
        case .stopped:
            return .off
        case .starting:
            return .starting
        case .meshing, .syncingViaRelay:
            return listening ? .available : .off
        }
    }

    /**
     Our own Shore Pass path.

     `RelayHealth.messageTooLarge` maps to `connected` on purpose: an oversized
     envelope is a fact about one message and one recipient, and the spec keeps
     it out of the path states entirely. The service is reachable and every
     other message is still moving; saying the pass is broken there is the old
     page's mistake.
     */
    static func relay(_ health: RelayHealth, configured: Bool) -> CoreRelayPathState {
        guard configured else { return .notSetUp }
        switch health {
        // A saved pass with no published verdict yet. That happens on every
        // cold start before the first check lands and again after the
        // controller tears its status down, and reading it as "not set up"
        // tells a person with a working pass to go and buy one -- which is
        // what the Shore Pass screen's own flicker machinery exists to avoid
        // saying.
        case .noConfig: return .checking
        case .checking: return .checking
        case .noInternet: return .waitingForInternet
        case .ok: return .connected
        case .failing: return .unreachable
        case .expired: return .passExpired
        case .suspended: return .passSuspended
        case .tokenRejected: return .setupRejected
        case .quotaFull: return .storageFull
        case .messageTooLarge: return .connected
        case .rateLimited: return .syncingSlowed
        }
    }

    /**
     `RelayHealth.noInternet` is the only "no validated internet" verdict the
     app publishes; every other health value was produced by a request that a
     validated network carried.
     */
    static func validatedInternet(_ health: RelayHealth) -> Bool {
        if case .noInternet = health { return false }
        return true
    }

    /// Last successful Shore Pass sync, or `0` when there has not been one.
    static func relayLastSyncMs(_ health: RelayHealth) -> Int64 {
        if case .ok(let lastSyncMs) = health { return lastSyncMs }
        return 0
    }

    static func directLink(_ path: DirectPath?) -> CoreDirectLink? {
        switch path {
        case .bluetooth: return .bluetooth
        case .localWifi: return .localWifi
        case nil: return nil
        }
    }
}

/**
 Holds the moment an unresolved check began, so the core can bound how long the
 card may say `Checking`.

 A single mutable value, because a mark that restarts on every render would
 make the bound unreachable and pin the card in Checking forever -- which is
 the failure the bound exists to prevent.
 */
final class CheckingClock {
    private var sinceMs: Int64 = 0

    /// - Returns: the epoch ms the current check started, or `0` when nothing
    ///   is pending.
    func mark(pending: Bool, nowMs: Int64) -> Int64 {
        if !pending {
            sinceMs = 0
            return 0
        }
        if sinceMs == 0 { sinceMs = nowMs }
        return sinceMs
    }
}

/**
 Is some path still coming up, with no verdict on it yet?

 The answer is the core's (`coreConnectionCheckPending`) because the same
 question is asked inside the classification, and a shell that asked a narrower
 one would start the bounded-Checking clock late -- or never -- and show a
 failure before the check that would prove it had finished. This platform's
 Bluetooth stack reports `.unknown` on a cold launch and `.resetting` when the
 radio is toggled, both of which arrive while the mesh is already meshing; a
 predicate that only looked at the runtime and the pass missed both.
 */
func connectionCheckPending(
    runtime: CoreMeshRuntime,
    bluetooth: CoreDirectPathState,
    localWifi: CoreDirectPathState,
    relay: CoreRelayPathState
) -> Bool {
    coreConnectionCheckPending(
        runtime: runtime,
        bluetooth: bluetooth,
        localWifi: localWifi,
        relay: relay
    )
}

// MARK: - Freshness and event times

/// How the health card's `Updated …` label reads.
enum FreshnessLabel: Equatable {
    /// Nothing has loaded yet, so there is nothing honest to date.
    case never
    case justNow
    case minutes(Int)
    case hours(Int)
}

/// How a recorded moment reads in a person row or an activity line.
enum EventTime: Equatable {
    /// Zero, negative, or otherwise unusable. Renders as no time at all.
    case unknown
    case justNow
    case minutes(Int)
    case hours(Int)
    case yesterday
    case older
}

enum ConnectionTimes {
    static let minuteMs: Int64 = 60_000
    static let hourMs: Int64 = 60 * 60_000
    static let dayMs: Int64 = 24 * 60 * 60_000

    /**
     The health card's freshness label.

     A snapshot stamped in the future is a clock artifact, not a reason to
     render a negative age, so it reads as just now.
     */
    static func freshness(updatedAtMs: Int64, nowMs: Int64) -> FreshnessLabel {
        if updatedAtMs <= 0 { return .never }
        let age = nowMs - updatedAtMs
        if age < minuteMs { return .justNow }
        if age < hourMs { return .minutes(Int(age / minuteMs)) }
        return .hours(Int(age / hourMs))
    }

    /**
     How one recorded moment reads.

     The spec asks for relative time inside a day, `Yesterday` when it applies,
     and a localized short date otherwise. Those two rules can disagree --
     8:03 PM yesterday seen at 10 AM today is under 24 hours old *and*
     yesterday -- so the calendar day wins, which is the reading the spec's own
     example ("Last connected yesterday at 8:03 PM") asks for.

     - Parameter startOfTodayMs: local midnight, supplied by the caller because
       a calendar needs a time zone and this file stays free of formatting.
     */
    static func eventTime(atMs: Int64, nowMs: Int64, startOfTodayMs: Int64) -> EventTime {
        if atMs <= 0 { return .unknown }
        if atMs >= nowMs { return .justNow }
        if atMs >= startOfTodayMs {
            let age = nowMs - atMs
            if age < minuteMs { return .justNow }
            if age < hourMs { return .minutes(Int(age / minuteMs)) }
            return .hours(Int(age / hourMs))
        }
        if atMs >= startOfTodayMs - dayMs { return .yesterday }
        return .older
    }
}

// MARK: - Refresh coalescing

/// The window a burst of store-change signals collapses into one reload.
let connectionCoalesceWindowMs: Int64 = 500

/**
 The page's reload policy: coalesced, single-flight, and never more than one
 follow-up owed.

 This page reads the same store and the same change stream that, undebounced
 and reloaded on the main actor, has already driven the app into a main-actor
 pileup during a mesh flood. Thousands of signals a minute is a normal
 condition here, not a stress test, so the policy is a first-class object with
 its own tests rather than a `Task.sleep` somewhere in a view.

 Holds no clock and no tasks: the caller passes `nowMs` and does the waiting,
 which is what makes the whole thing testable.
 */
final class StoreChangeCoalescer {
    private let windowMs: Int64
    private var inFlight = false
    private var followUp = false
    private var windowEndsAtMs: Int64?

    init(windowMs: Int64 = connectionCoalesceWindowMs) {
        self.windowMs = windowMs
    }

    /**
     A store-change signal arrived.

     - Returns: true when the caller now owns a reload window and should wait it
       out (`remainingMs`) before loading; false when this signal was absorbed
       -- either by a window already open, or by a reload already running, in
       which case exactly one follow-up is remembered.
     */
    func onSignal(nowMs: Int64) -> Bool {
        if inFlight {
            followUp = true
            return false
        }
        if windowEndsAtMs != nil { return false }
        windowEndsAtMs = nowMs + windowMs
        return true
    }

    /**
     How long is still owed to the open window; `0` once it has elapsed.

     Clamped to the window length so a clock that jumps backwards cannot stall
     the page behind an enormous wait.
     */
    func remainingMs(nowMs: Int64) -> Int64 {
        guard let ends = windowEndsAtMs else { return 0 }
        return min(max(ends - nowMs, 0), windowMs)
    }

    func onReloadStarted() {
        windowEndsAtMs = nil
        inFlight = true
        // Signals that arrived during the wait are already covered by the load
        // that is about to read the store; only ones arriving from here on are
        // owed a follow-up.
        followUp = false
    }

    /// - Returns: true when at least one signal arrived mid-reload and is owed
    ///   a follow-up.
    func onReloadFinished() -> Bool {
        inFlight = false
        let owed = followUp
        followUp = false
        return owed
    }

    /**
     Forget any window or reload this object still believes is outstanding.

     Called when the loop that drives it starts and stops, because the loop is
     cancelled every time the page goes away and this object outlives it.
     Without the reset it would spend the rest of its life absorbing every
     signal as "a reload is already running", and the page would never load
     again: frozen rows, a freshness label that keeps ageing, and a
     pull-to-refresh spinner with nothing behind it.
     */
    func reset() {
        inFlight = false
        followUp = false
        windowEndsAtMs = nil
    }
}

// MARK: - Delivery language

enum DeliveryPresentation {
    /**
     The neutral delivery line for one person, or nil when there is nothing
     honest to say.

     The decision is the core's (`coreClassifyDeliveryLine`) -- including the
     route-usability predicate and the rule that decides whether the
     relay-upload backlog says anything about delivery at all. All that happens
     here is handing over this platform's facts and pairing the answer with its
     count.

     Note what `queued` is *not*: it is not receipt-aware. It counts outbound
     rows whose upload stamp is unset, and only an upload sets that stamp --
     not a delivery receipt, and not handing the message over in person. The
     core gates on that; see `core_relay_queue_reflects_delivery`.

     - Parameter routeIsDirect: a live direct link to this person exists right now.
     - Parameter ownRelayUsable: our own Shore Pass path can deliver
       (`CoreConnectionEvidence.ownRelayUsable`).
     - Parameter contactHasRelayEndpoint: their friend card carries an
       internet-delivery endpoint at all. Without one, no amount of internet on
       this phone reaches them.
     - Parameter contactRelayStale: their endpoint has been written off after
       authoritatively rejecting us, so it is not a usable route today.
     - Parameter receiptIsNewestEvidence: the freshest thing recorded about
       this person is a delivery receipt -- their row already says they
       received a message from us.
     */
    static func line(
        queued: Int,
        routeIsDirect: Bool,
        ownRelayUsable: Bool,
        contactHasRelayEndpoint: Bool,
        contactRelayStale: Bool,
        relay: CoreRelayPathState,
        receiptIsNewestEvidence: Bool
    ) -> DeliveryLine? {
        if queued <= 0 { return nil }
        let state = coreClassifyDeliveryLine(
            input: CoreDeliveryLineInput(
                queued: UInt32(queued),
                relay: relay,
                ownRelayUsable: ownRelayUsable,
                contactHasRelayEndpoint: contactHasRelayEndpoint,
                contactRelayStale: contactRelayStale,
                directLink: routeIsDirect,
                deliveryReceiptIsNewestEvidence: receiptIsNewestEvidence
            )
        )
        guard let state = state else { return nil }
        return DeliveryLine(kind: state, count: queued)
    }
}

// MARK: - View-state assembly

/// Contacts read per reload. The address book is small; this is the ceiling, not a page size.
let connectionPeopleLimit = 200

/// Connection events read per reload. Ten are shown; the rest back `Show all activity`.
let connectionActivityQueryLimit: UInt32 = 50

/// Events shown while Recent activity is not expanded to everything.
let connectionActivityPreviewCount = 10

/// Rows shown before Other people collapses behind a `Show N people` control.
let connectionOtherPeopleCollapseAt = 5

enum ConnectionDetailsLogic {

    /**
     Turn live signals plus the last store snapshot into everything the page
     renders.

     Both classifications come from the core and neither is second-guessed
     here. In particular the relay state the health card reports is the
     *normalized* one from the core's evidence, and the Paths row renders that
     same value -- which is what stops the page claiming Shore Pass is
     connected on a phone that has been offline for an hour.

     Phase 1 supplies no per-person attention reason, so the core's Needs
     attention group is structurally empty and the page shows two groups. The
     machinery that fills it in is the per-recipient delivery read model, which
     is Phase 2.
     */
    static func buildState(
        runtimeState: MeshRuntimeState,
        bluetoothAvailability: BluetoothAvailability,
        directPaths: [Data: DirectPath],
        relayHealth: RelayHealth,
        relayConfigured: Bool,
        lanListening: Bool,
        bluetoothAudioActive: Bool,
        staleRelayContacts: Set<Data>,
        presenceLastSeen: [Data: Int64],
        contactLastSeen: [Data: Int64],
        snapshot: ConnectionStoreSnapshot,
        checkingSinceMs: Int64,
        refreshing: Bool,
        nowMs: Int64
    ) -> ConnectionDetailsState {
        let people = snapshot.people
        // Only friends count as "nearby": a stranger's phone HELLO'ing past is
        // not someone this page can promise anything about. Blocked identities
        // are not friends either -- a block is a tombstone, and a count that
        // only a blocked person produces ("1 friend nearby" above a People
        // section with nobody in it) discloses their presence just as surely as
        // a row would.
        let visibleIds = Set(people.filter { !$0.blocked }.map { $0.userId })
        var friendPaths: [Data: DirectPath] = [:]
        for (userId, path) in directPaths where visibleIds.contains(userId) {
            friendPaths[userId] = path
        }
        let bluetoothLinks = friendPaths.values.filter { $0 == .bluetooth }.count
        let localWifiLinks = friendPaths.values.filter { $0 == .localWifi }.count

        let runtime = ConnectionInputs.runtime(runtimeState, bluetooth: bluetoothAvailability)
        let relayPath = ConnectionInputs.relay(relayHealth, configured: relayConfigured)
        let input = CoreConnectionHealthInput(
            runtime: runtime,
            bluetooth: ConnectionInputs.bluetooth(runtimeState, availability: bluetoothAvailability),
            bluetoothLinks: UInt32(bluetoothLinks),
            localWifi: ConnectionInputs.localWifi(runtimeState, listening: lanListening),
            localWifiLinks: UInt32(localWifiLinks),
            relay: relayPath,
            validatedInternet: ConnectionInputs.validatedInternet(relayHealth),
            nearbyFriendCount: UInt32(friendPaths.count),
            checkingSinceMs: checkingSinceMs,
            nowMs: nowMs
        )
        let report = coreClassifyConnectionHealth(input: input)
        let evidence = report.evidence

        let inputs: [CorePersonHealthInput] = people.map { person in
            CorePersonHealthInput(
                userId: person.userId,
                displayName: person.name,
                blocked: person.blocked,
                directLink: ConnectionInputs.directLink(directPaths[person.userId]),
                presenceLastSeenMs: presenceLastSeen[person.userId] ?? 0,
                lastSeenMs: max(contactLastSeen[person.userId] ?? 0, person.latest?.atMs ?? 0),
                // Phase 1 has no per-person attention machinery; see the doc
                // comment above.
                attention: nil,
                attentionSinceMs: 0
            )
        }
        let placements = coreGroupPeople(
            people: inputs,
            ownRelayUsable: evidence.ownRelayUsable,
            nowMs: nowMs
        )

        var byId: [Data: ConnectionPerson] = [:]
        for person in people { byId[person.userId] = person }

        func rows(for group: [CorePersonPlacement]) -> [ConnectionPersonRow] {
            group.compactMap { placement -> ConnectionPersonRow? in
                guard let person = byId[placement.userId] else { return nil }
                return ConnectionDetailsLogic.personRow(
                    person: person,
                    reach: placement.reach,
                    presenceLastSeenMs: presenceLastSeen[person.userId] ?? 0,
                    ownRelayUsable: evidence.ownRelayUsable,
                    relay: evidence.relay,
                    stale: staleRelayContacts.contains(person.userId)
                )
            }
        }

        let health = HealthCardState(
            state: report.state,
            nearbyFriendCount: friendPaths.count,
            bluetooth: evidence.bluetooth,
            relay: evidence.relay,
            reason: report.reason,
            action: report.action
        )
        let paths = PathsCardState(
            bluetooth: evidence.bluetooth,
            bluetoothLinks: bluetoothLinks,
            bluetoothAudioActive: bluetoothAudioActive,
            localWifiLinks: localWifiLinks,
            relay: evidence.relay,
            relayLastSyncMs: ConnectionInputs.relayLastSyncMs(relayHealth)
        )
        return ConnectionDetailsState(
            health: health,
            paths: paths,
            // Needs attention is empty by construction in Phase 1; if a later
            // change ever fills it, surfacing it here beside Reachable now
            // would be wrong, so it is deliberately not merged in.
            reachableNow: rows(for: placements.reachableNow),
            otherPeople: rows(for: placements.otherPeople),
            hasContacts: people.contains { !$0.blocked },
            activity: snapshot.activity,
            updatedAtMs: snapshot.loadedAtMs,
            refreshing: refreshing
        )
    }

    private static func personRow(
        person: ConnectionPerson,
        reach: CorePersonReach,
        presenceLastSeenMs: Int64,
        ownRelayUsable: Bool,
        relay: CoreRelayPathState,
        stale: Bool
    ) -> ConnectionPersonRow {
        let status: PersonStatus
        let badge: ConnectionPathBadge?
        switch reach {
        case .directBluetooth:
            status = .connectedNow
            badge = .bluetooth
        case .directLocalWifi:
            status = .connectedNow
            badge = .localWifi
        case .relayPresence:
            status = .seenOnline(atMs: presenceLastSeenMs)
            badge = .shorePass
        // Spelled out: bare `.none` beside an optional in the same file reads
        // as `Optional.none` to a person even where it does not to the
        // compiler.
        case CorePersonReach.none:
            if let latest = person.latest {
                status = .history(evidence: latest.evidence, atMs: latest.atMs)
                badge = latest.path
            } else {
                status = .noHistory
                badge = nil
            }
        }
        let isDirect = reach == .directBluetooth || reach == .directLocalWifi
        return ConnectionPersonRow(
            userIdHex: person.userIdHex,
            name: person.name,
            status: status,
            badge: badge,
            delivery: DeliveryPresentation.line(
                queued: person.queued,
                routeIsDirect: isDirect,
                ownRelayUsable: ownRelayUsable,
                contactHasRelayEndpoint: person.hasRelayEndpoint,
                contactRelayStale: stale,
                relay: relay,
                receiptIsNewestEvidence: person.latest?.evidence == .messageDelivered
            )
        )
    }

    /**
     The badge for an observed path, or nil when no path was observed.

     Nil exactly for a carried arrival: another phone brought the message the
     last hop, so naming a radio here would claim the friend was in range when
     they may be nowhere near. Pinned against the core's own
     `corePeerTransportIsObserved` by `ConnectionActivityLogicTests`, so the
     two cannot drift apart. Mirrors `badgeFor` in ConnectionDetailsLogic.kt.
     */
    static func observedPath(_ transport: PeerConnectionTransport) -> ConnectionPathBadge? {
        switch transport {
        case .bluetooth: return .bluetooth
        case .localWifi: return .localWifi
        case .shorePass: return .shorePass
        case .carried: return nil
        }
    }
}
