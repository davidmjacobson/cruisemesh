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
 The informational expansion under a person row.

 Everything here is a restatement, never a control: the spec forbids a manual
 transport picker, and `bestRoute` in particular is the core's routing answer
 (`corePersonBestRoute`) rather than anything this page worked out. A page that
 re-derived reachability from "can I poll them" would report post-only friend
 cards as broken, which is the failure the core answer exists to prevent.
 */
struct PersonDetail: Equatable {
    let bestRoute: CorePersonRoute
    /// Freshest evidence their device was alive, epoch ms; `0` when none.
    let lastSeenMs: Int64
    /// Their delivery receipt for one of our messages, epoch ms; `0` when none.
    let lastDeliveredMs: Int64
}

struct ConnectionPersonRow: Equatable {
    let userIdHex: String
    let name: String
    let status: PersonStatus
    let badge: ConnectionPathBadge?
    /**
     What is still outstanding for this person, as the core classified it
     (`coreClassifyRecipientDelivery`); nil when there is nothing to say.
     */
    let delivery: CoreDeliveryLine?
    /**
     Why they are in Needs attention. The same verdict as `delivery`'s, since
     both come out of the one classification, so a row cannot sit in Needs
     attention over a problem its own delivery line does not mention.
     */
    let attention: CorePersonAttention?
    let detail: PersonDetail
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

/**
 Everything the page renders, as one finished value.

 `Equatable` on purpose: this is republished whenever any observable moves, and
 at mesh-flood rates most of those moves change nothing anybody can see. An
 equal rebuild is dropped rather than redrawing the whole page for a last-seen
 timestamp that rounds to the same minute.
 */
struct ConnectionDetailsState: Equatable {
    let health: HealthCardState
    let paths: PathsCardState
    let needsAttention: [ConnectionPersonRow]
    let reachableNow: [ConnectionPersonRow]
    let otherPeople: [ConnectionPersonRow]
    let hasContacts: Bool
    let activity: [ConnectionActivityRow]
    /// Epoch ms the snapshot behind this state was loaded; `0` before the first load.
    let updatedAtMs: Int64
    let refreshing: Bool

    /**
     What the page shows before anything has been classified.

     Deliberately `checking` rather than `ready`: the model derives the real
     state the moment it starts, and a placeholder that claimed the phone was
     working normally would be a verdict nobody has reached yet. `updatedAtMs`
     of zero keeps the freshness label and the "no friends added yet" line off
     the screen until a snapshot actually exists.
     */
    static let checking = ConnectionDetailsState(
        health: HealthCardState(
            state: .checking,
            nearbyFriendCount: 0,
            bluetooth: .starting,
            relay: .checking,
            reason: nil,
            action: nil
        ),
        paths: PathsCardState(
            bluetooth: .starting,
            bluetoothLinks: 0,
            bluetoothAudioActive: false,
            localWifiLinks: 0,
            relay: .checking,
            relayLastSyncMs: 0
        ),
        needsAttention: [],
        reachableNow: [],
        otherPeople: [],
        hasContacts: false,
        activity: [],
        updatedAtMs: 0,
        refreshing: false
    )
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

/**
 What one person's outgoing mail looks like, straight from the core's
 per-recipient read model (`MessageStore.recipientDeliveryStatus`).

 Facts only, and every one of them is passed to the core untouched. In
 particular the four endpoint-health numbers are *not* interpreted here: the
 streak thresholds and rest windows belong to `contact_relay_health`, and a
 shell that reproduced any of them would be the start of the next drift.
 */
struct PersonDeliveryFacts: Equatable {
    let waitingCount: Int
    /**
     How much of `waitingCount` this phone has not managed to hand over yet.

     Zero, with messages still waiting, means this phone has done everything it
     can and the other one has not collected -- ordinary store-and-forward,
     never a stall. The core gates the delayed line on it.
     */
    let unpostedWaitingCount: Int
    let oldestWaitingMs: Int64
    let lastProgressMs: Int64
    let oversizedWaiting: Bool
    let relayRejectStreak: Int64
    let relayRejectedAtMs: Int64
    let relayUnreachableStreak: Int64
    let relayUnreachableAtMs: Int64

    /// Nothing outstanding and no endpoint trouble: the ordinary case.
    static let none = PersonDeliveryFacts(
        waitingCount: 0,
        unpostedWaitingCount: 0,
        oldestWaitingMs: 0,
        lastProgressMs: 0,
        oversizedWaiting: false,
        relayRejectStreak: 0,
        relayRejectedAtMs: 0,
        relayUnreachableStreak: 0,
        relayUnreachableAtMs: 0
    )
}

/// One person as the background load found them.
struct ConnectionPerson: Equatable {
    let userId: Data
    let userIdHex: String
    let name: String
    let blocked: Bool
    /// Their friend card carries an internet-delivery endpoint.
    let hasRelayEndpoint: Bool
    let delivery: PersonDeliveryFacts
    /// Newest recorded evidence across every path, or nil when there is none.
    let latest: PersonEvidence?
    /// Their newest delivery receipt for one of our messages; `0` when none.
    let lastDeliveredMs: Int64
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
        case .deferredRoaming: return .waitingForInternet
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
        if case .deferredRoaming = health { return false }
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

/**
 How long a message has been waiting, for the `· 14 min` half of a delayed or
 blocked delivery line.

 Deliberately not `EventTime`: that renders a *moment* ("14 min ago",
 "yesterday at 8:03 PM"), and an age is a duration. Reusing it would put "ago"
 inside a sentence that already reads as elapsed time, and would eventually put
 a calendar date there -- `2 messages delayed · on 3/14/26` says nothing a
 reader can use.
 */
enum WaitingAge: Equatable {
    /// Unusable or under a minute. The line renders with no age at all.
    case unknown
    case minutes(Int)
    case hours(Int)
    case days(Int)
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

    /**
     How long something queued at `sinceMs` has been waiting.

     An unset stamp and a stamp in the future both come back `.unknown`: the
     second is a clock artifact, and the alternative is a negative age rendered
     as an enormous one. Under a minute is `.unknown` too, because `2 messages
     delayed · 0 min` reads as a bug.
     */
    static func waitingAge(sinceMs: Int64, nowMs: Int64) -> WaitingAge {
        if sinceMs <= 0 || nowMs < sinceMs { return .unknown }
        let age = nowMs - sinceMs
        if age < minuteMs { return .unknown }
        if age < hourMs { return .minutes(Int(age / minuteMs)) }
        if age < dayMs { return .hours(Int(age / hourMs)) }
        return .days(Int(age / dayMs))
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
     The delivery verdict for one person, or nil when there is nothing honest
     to say.

     Every part of the decision is the core's (`coreClassifyRecipientDelivery`)
     -- the route-usability predicate, the delayed window, which faults become
     an error row, and which of those puts a person in Needs attention. All
     that happens here is handing over the store's facts and this device's path
     state.

     The count arriving here is already receipt-aware, which is what makes
     "Received your message 12 min ago" and a waiting line unable to appear
     together: not a special case that suppresses the second, but nothing left
     to count. The Phase 1 front end that had to suppress it by hand
     (`coreClassifyDeliveryLine`) is now called by neither shell; it stays
     exported, and pinned by its own Rust test, as the documented narrow door
     onto the one decision procedure.

     - Parameter directLink: a live direct link to this person exists right now.
     - Parameter ownRelayUsable: our own Shore Pass path can deliver
       (`CoreConnectionEvidence.ownRelayUsable`).
     - Parameter relay: this phone's normalized Shore Pass path
       (`CoreConnectionEvidence.relay`).
     */
    static func line(
        person: ConnectionPerson,
        directLink: Bool,
        ownRelayUsable: Bool,
        relay: CoreRelayPathState,
        nowMs: Int64
    ) -> CoreDeliveryLine? {
        coreClassifyRecipientDelivery(
            input: CoreRecipientDeliveryInput(
                // Clamped rather than wrapped: the core counts up from zero,
                // and a shell that let a negative fold into an unsigned value
                // would put an absurd number under someone's name.
                waitingCount: UInt32(clamping: person.delivery.waitingCount),
                unpostedWaitingCount: UInt32(clamping: person.delivery.unpostedWaitingCount),
                oldestWaitingMs: person.delivery.oldestWaitingMs,
                lastProgressMs: person.delivery.lastProgressMs,
                oversizedWaiting: person.delivery.oversizedWaiting,
                relayRejectStreak: person.delivery.relayRejectStreak,
                relayRejectedAtMs: person.delivery.relayRejectedAtMs,
                relayUnreachableStreak: person.delivery.relayUnreachableStreak,
                relayUnreachableAtMs: person.delivery.relayUnreachableAtMs,
                relay: relay,
                ownRelayUsable: ownRelayUsable,
                contactHasRelayEndpoint: person.hasRelayEndpoint,
                directLink: directLink,
                nowMs: nowMs
            )
        )
    }

    /**
     How a message to this person would travel right now, asked of the core
     rather than worked out here.

     The endpoint-resting half is `coreContactEndpointResting`, the same
     predicate the delivery classification consults, so the person detail's
     route sentence and the delivery line under their name are two readings of
     one answer.
     */
    static func bestRoute(
        person: ConnectionPerson,
        directLink: CoreDirectLink?,
        ownRelayUsable: Bool,
        nowMs: Int64
    ) -> CorePersonRoute {
        corePersonBestRoute(
            directLink: directLink,
            ownRelayUsable: ownRelayUsable,
            contactHasRelayEndpoint: person.hasRelayEndpoint,
            contactEndpointResting: coreContactEndpointResting(
                relayRejectStreak: person.delivery.relayRejectStreak,
                relayRejectedAtMs: person.delivery.relayRejectedAtMs,
                relayUnreachableStreak: person.delivery.relayUnreachableStreak,
                relayUnreachableAtMs: person.delivery.relayUnreachableAtMs,
                nowMs: nowMs
            )
        )
    }
}

/**
 What a `How to fix` control opens onto.

 Two sources, one destination: the health card's device-wide fault and a person
 row's per-recipient one. They are separate types in the core because one is
 about this phone and the other about one friend -- the distinction that stops
 a friend's broken card turning the whole page red -- and the shell keeps them
 apart for the same reason rather than flattening them into a single reason
 enum.
 */
enum HowToFixTopic: Equatable, Identifiable {
    /// A fault with this device's own connection.
    case device(CoreHealthReason)
    /// A fault stopping delivery to one friend, named so the copy can say who.
    case person(reason: CoreDeliveryBlockedReason, name: String)

    /// SwiftUI presents a sheet from an `Identifiable` item; the identity is
    /// the topic itself, so re-tapping the same reason does not re-present.
    var id: String {
        switch self {
        case .device(let reason):
            return "device-\(reason)"
        case .person(let reason, let name):
            return "person-\(reason)-\(name)"
        }
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

/**
 Recent events shown inside one person's expansion.

 The spec's number, and it is also why this query is not part of the page
 reload: five rows for one person, read once when a reader asks for them, is a
 bounded cost that does not multiply by the address book.
 */
let connectionPersonEventLimit: UInt32 = 5

enum ConnectionDetailsLogic {

    /**
     Turn live signals plus the last store snapshot into everything the page
     renders.

     All three classifications come from the core and none is second-guessed
     here. In particular the relay state the health card reports is the
     *normalized* one from the core's evidence, and the Paths row renders that
     same value -- which is what stops the page claiming Shore Pass is
     connected on a phone that has been offline for an hour.

     The order below is load-bearing. Each person's delivery is classified
     *before* the grouping call, and the attention it produces is what the
     grouping is given. That is what makes a Needs attention row and its own
     delivery line the same verdict rather than two judgements that can
     disagree -- a person cannot be filed under a problem their row does not
     state.
     */
    static func buildState(
        runtimeState: MeshRuntimeState,
        bluetoothAvailability: BluetoothAvailability,
        directPaths: [Data: DirectPath],
        relayHealth: RelayHealth,
        relayConfigured: Bool,
        lanListening: Bool,
        bluetoothAudioActive: Bool,
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

        // Classified once per person per reload, and reused for the grouping,
        // the row, and the expansion -- not recomputed at each of those points,
        // which would be three FFI calls per friend.
        var deliveryById: [Data: CoreDeliveryLine] = [:]
        for person in people {
            let line = DeliveryPresentation.line(
                person: person,
                directLink: directPaths[person.userId] != nil,
                ownRelayUsable: evidence.ownRelayUsable,
                relay: evidence.relay,
                nowMs: nowMs
            )
            if let line = line { deliveryById[person.userId] = line }
        }

        let inputs: [CorePersonHealthInput] = people.map { person in
            let delivery = deliveryById[person.userId]
            return CorePersonHealthInput(
                userId: person.userId,
                displayName: person.name,
                blocked: person.blocked,
                directLink: ConnectionInputs.directLink(directPaths[person.userId]),
                presenceLastSeenMs: presenceLastSeen[person.userId] ?? 0,
                lastSeenMs: max(contactLastSeen[person.userId] ?? 0, person.latest?.atMs ?? 0),
                attention: delivery?.attention,
                attentionSinceMs: delivery?.oldestWaitingMs ?? 0
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
                    delivery: deliveryById[person.userId],
                    bestRoute: DeliveryPresentation.bestRoute(
                        person: person,
                        directLink: ConnectionInputs.directLink(directPaths[person.userId]),
                        ownRelayUsable: evidence.ownRelayUsable,
                        nowMs: nowMs
                    ),
                    lastSeenMs: max(
                        contactLastSeen[person.userId] ?? 0,
                        presenceLastSeen[person.userId] ?? 0,
                        person.latest?.atMs ?? 0
                    )
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
            needsAttention: rows(for: placements.needsAttention),
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
        delivery: CoreDeliveryLine?,
        bestRoute: CorePersonRoute,
        lastSeenMs: Int64
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
        return ConnectionPersonRow(
            userIdHex: person.userIdHex,
            name: person.name,
            status: status,
            badge: badge,
            delivery: delivery,
            attention: delivery?.attention,
            detail: PersonDetail(
                bestRoute: bestRoute,
                lastSeenMs: lastSeenMs,
                lastDeliveredMs: person.lastDeliveredMs
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
