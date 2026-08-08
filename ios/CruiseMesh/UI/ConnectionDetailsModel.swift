import CoreBluetooth
import Foundation

/// Wall clock and calendar helpers the page needs. Separated so the pure logic
/// stays clock-free and the tests can pass their own instants.
enum ConnectionClock {
    static var nowMs: Int64 {
        Int64(Date().timeIntervalSince1970 * 1_000)
    }

    /// Local midnight for `nowMs`; the calendar boundary `yesterday` is
    /// measured from.
    static func startOfDayMs(_ nowMs: Int64, calendar: Calendar = .current) -> Int64 {
        let date = Date(timeIntervalSince1970: TimeInterval(nowMs) / 1_000)
        return Int64(calendar.startOfDay(for: date).timeIntervalSince1970 * 1_000)
    }
}

extension BluetoothAvailability {
    /**
     What the CoreBluetooth central reports, in the page's terms.

     `unknown` and `resetting` are `starting`, not `off`: the radio answers a
     fraction of a second after launch, and a page that called that "Bluetooth
     is off" would be wrong every time it opened during those milliseconds.
     */
    static func observed(
        authorizationBlocked: Bool,
        radioState: CBManagerState
    ) -> BluetoothAvailability {
        if authorizationBlocked { return .off }
        switch radioState {
        case .poweredOn: return .available
        case .poweredOff, .unsupported, .unauthorized: return .off
        case .unknown, .resetting: return .starting
        @unknown default: return .starting
        }
    }
}

/**
 Everything this page needs from the store, in one bounded pass.

 Runs off the main actor only. Every query is limited: contacts to
 `connectionPeopleLimit`, events to `connectionActivityQueryLimit`. Nothing
 here scales with total history size.

 Blocked identities are dropped from Recent activity here and from the People
 groups by the core, so a block is honoured on this page in both directions.
 The block tombstones come from one query rather than a per-contact question: a
 block can outlive the contact row and can sort past the people cap, and either
 way an activity row for a blocked identity is the tombstone leaking.

 Mirrors `loadConnectionSnapshot` in ConnectionDetailsScreen.kt.
 */
enum ConnectionSnapshotLoader {
    static func load(nowMs: Int64) -> ConnectionStoreSnapshot {
        let store = AppStore.get()
        let allContacts: [Contact] = (try? store.listContacts()) ?? []
        let contacts = Array(allContacts.prefix(connectionPeopleLimit))
        let blocked = Set((try? store.listBlockedUsers()) ?? [])

        let depthRows: [RelayQueueDepth] =
            (try? store.pendingRelayOutboundDepthByRecipient(nowMs: nowMs)) ?? []
        var depths: [Data: Int] = [:]
        for row in depthRows {
            depths[row.recipientUserId] = Int(row.queued)
        }

        let summaryRows: [PeerConnectionSummary] = (try? store.peerConnectionSummaries()) ?? []
        var summaries: [Data: [PeerConnectionSummary]] = [:]
        for row in summaryRows {
            summaries[row.userId, default: []].append(row)
        }

        let people: [ConnectionPerson] = contacts.map { contact in
            let latest = ConnectionActivityLogic.latestPeerStatus(summaries[contact.userId] ?? [])
            let relayUrl = (contact.relayUrl ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            var evidence: PersonEvidence?
            if let latest = latest {
                evidence = PersonEvidence(
                    evidence: latest.evidence,
                    path: ConnectionDetailsLogic.observedPath(latest.transport),
                    atMs: latest.atMs
                )
            }
            return ConnectionPerson(
                userId: contact.userId,
                userIdHex: UserIdHex.encode(contact.userId),
                name: coreContactDisplayName(contact: contact),
                blocked: blocked.contains(contact.userId),
                hasRelayEndpoint: !relayUrl.isEmpty,
                queued: depths[contact.userId] ?? 0,
                latest: evidence
            )
        }

        var names: [Data: String] = [:]
        for person in people {
            names[person.userId] = person.name
        }

        let events: [PeerConnectionEvent] = (try? store.peerConnectionEvents(
            userId: nil,
            limit: connectionActivityQueryLimit
        )) ?? []
        var activity: [ConnectionActivityRow] = []
        activity.reserveCapacity(events.count)
        for event in events where !blocked.contains(event.userId) {
            activity.append(
                ConnectionActivityRow(
                    name: names[event.userId],
                    evidence: ConnectionActivityLogic.evidence(of: event.kind),
                    path: ConnectionDetailsLogic.observedPath(event.transport),
                    atMs: event.occurredAtMs
                )
            )
        }

        return ConnectionStoreSnapshot(people: people, activity: activity, loadedAtMs: nowMs)
    }
}

/**
 How often the polling fallback asks for a reload while the page is visible.

 Four seconds, not five: the coalescing window adds up to another 500 ms before
 a load even starts, and the acceptance criterion is that a newly recorded
 connection event appears *within* five. A tick exactly at the budget spends the
 whole of it and then some.
 */
private let storePollIntervalMs: Int64 = 4_000

/**
 How often relative times, the freshness label, and the bounded Checking state
 are re-evaluated.

 The spec asks for the freshness label to move at least once a minute; ten
 seconds is faster because the same clock also decides when `Checking`
 resolves, and a card that stayed on a spinner for most of a minute after the
 bound expired would defeat the bound.
 */
private let clockTickMs: Int64 = 10_000

/**
 The page's refresh engine: what to reload, when, and on which actor.

 Live signals (runtime, transports, relay health, presence) come straight off
 their existing observable objects and land on screen within a frame; this
 object owns only the parts that need the store.

 The rules are the spec's, and they are not decoration. This screen reads the
 same store and the same change stream that has already contributed to a
 main-actor pileup during a mesh flood:

 - **Coalesce**: a burst of signals collapses into one reload
   (`StoreChangeCoalescer`, 500 ms).
 - **Never on main**: the load runs in a detached task and only the finished
   snapshot comes back to the main actor.
 - **Single flight**: one consumer loop, so at most one reload is in progress,
   and exactly one follow-up is owed for signals that arrive mid-reload.
 - **Bounded**: every query the load runs is limited.

 `stop()` tears all of it down, so navigating away ends every page-driven task.
 */
@MainActor
final class ConnectionDetailsModel: ObservableObject {
    @Published private(set) var snapshot = ConnectionStoreSnapshot.empty
    @Published private(set) var isRefreshing = false
    /// Ticks while the page is visible, so relative times and the freshness
    /// label age on screen instead of freezing at the last store change.
    @Published private(set) var nowMs: Int64 = ConnectionClock.nowMs

    /// Bounds how long the health card may say `Checking`. Owned here because
    /// a mark that restarted on every render would make the bound unreachable.
    let checkingClock = CheckingClock()

    private let coalescer = StoreChangeCoalescer()
    private var requests: AsyncStream<Void>.Continuation?
    private var loopTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?
    private var clockTask: Task<Void, Never>?
    private var pullWaiters: [CheckedContinuation<Void, Never>] = []

    /// Nonisolated so `@StateObject private var model = ConnectionDetailsModel()`
    /// needs no actor hop to build the view.
    nonisolated init() {}

    var isRunning: Bool { loopTask != nil }

    func start() {
        guard loopTask == nil else { return }
        // A previous run may have been torn down mid-window or mid-load.
        coalescer.reset()

        let stream = AsyncStream<Void>(Void.self, bufferingPolicy: .bufferingNewest(1)) {
            continuation in
            self.requests = continuation
        }
        // Unstructured `Task` inherits this main-actor context, so the loop
        // itself stays on the main actor and only the store read leaves it.
        loopTask = Task { [weak self] in
            for await _ in stream {
                guard let self = self else { return }
                await self.waitOutCoalescingWindow()
                if Task.isCancelled { return }
                await self.reload()
            }
        }
        pollTask = Task { [weak self] in
            // Polling fallback until the store publishes a change signal. The
            // same three rules apply: a tick that lands mid-reload is
            // absorbed, not stacked.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(storePollIntervalMs) * 1_000_000)
                // Not `self?.` -- a deallocated model with no `stop()` behind
                // it would otherwise leave this timer looping forever doing
                // nothing at all.
                guard let self = self else { return }
                if Task.isCancelled { return }
                self.signalStoreChanged()
            }
        }
        clockTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self = self else { return }
                self.nowMs = ConnectionClock.nowMs
                try? await Task.sleep(nanoseconds: UInt64(clockTickMs) * 1_000_000)
            }
        }

        // Seed with a window that has already elapsed: the first paint should
        // not wait out a debounce nobody asked for.
        if coalescer.onSignal(nowMs: ConnectionClock.nowMs - connectionCoalesceWindowMs) {
            requests?.yield()
        }
    }

    func stop() {
        loopTask?.cancel()
        loopTask = nil
        pollTask?.cancel()
        pollTask = nil
        clockTask?.cancel()
        clockTask = nil
        requests?.finish()
        requests = nil
        isRefreshing = false
        // A reload cancelled mid-flight would otherwise leave the coalescer
        // believing one is still running, and every later signal would be
        // absorbed as a follow-up that never comes.
        coalescer.reset()
        resumePullWaiters()
    }

    /// Something that this page reads may have changed in the store.
    func signalStoreChanged() {
        guard loopTask != nil else { return }
        if coalescer.onSignal(nowMs: ConnectionClock.nowMs) {
            requests?.yield()
        }
    }

    /**
     Pull-to-refresh: reload the snapshot and ask for exactly one bounded
     connectivity re-check.

     This is the one deliberate "check again now" affordance on the page, and
     the only way the page influences radio or sync behavior. It returns once a
     reload has finished so the pull indicator matches real work; a background
     poll never drives that indicator.
     */
    func refreshFromPull() async {
        RelaySyncEvents.requestSync()
        guard loopTask != nil else { return }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            pullWaiters.append(continuation)
            signalStoreChanged()
        }
    }

    private func waitOutCoalescingWindow() async {
        var remaining = coalescer.remainingMs(nowMs: ConnectionClock.nowMs)
        while remaining > 0 {
            try? await Task.sleep(nanoseconds: UInt64(remaining) * 1_000_000)
            if Task.isCancelled { return }
            remaining = coalescer.remainingMs(nowMs: ConnectionClock.nowMs)
        }
    }

    private func reload() async {
        coalescer.onReloadStarted()
        isRefreshing = true
        let loadedAtMs = ConnectionClock.nowMs
        // The only place this page touches the store, and it is never the main
        // actor.
        let loaded = await Task.detached(priority: .userInitiated) {
            ConnectionSnapshotLoader.load(nowMs: loadedAtMs)
        }.value
        // The last good snapshot stayed on screen throughout; it is replaced
        // only once a whole new one exists.
        snapshot = loaded
        nowMs = ConnectionClock.nowMs
        isRefreshing = false
        resumePullWaiters()
        if coalescer.onReloadFinished() { signalStoreChanged() }
    }

    private func resumePullWaiters() {
        let waiters = pullWaiters
        pullWaiters = []
        for waiter in waiters { waiter.resume() }
    }
}
