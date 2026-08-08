import Combine
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
    static func load(ownUserId: Data, nowMs: Int64) -> ConnectionStoreSnapshot {
        let store = AppStore.get()
        let allContacts: [Contact] = (try? store.listContacts()) ?? []
        let contacts = Array(allContacts.prefix(connectionPeopleLimit))
        let blocked = Set((try? store.listBlockedUsers()) ?? [])

        // The per-recipient read model, not the relay-upload backlog. The
        // backlog is a diagnostic that never drains on a phone with no pass;
        // this is receipt-aware, which is why a row saying a friend received a
        // message can no longer have a waiting line under it. Blocked
        // identities are dropped inside the query, so the map simply has no
        // entry for them.
        let statusRows: [CoreRecipientDeliveryStatus] = (try? store.recipientDeliveryStatus(
            ownUserId: ownUserId,
            recipientUserIds: contacts.map { $0.userId },
            nowMs: nowMs
        )) ?? []
        var deliveryFacts: [Data: PersonDeliveryFacts] = [:]
        for row in statusRows {
            deliveryFacts[row.recipientUserId] = PersonDeliveryFacts(
                // Clamped rather than truncated: the core counts up from zero,
                // and a shell that let an out-of-range value fold into a
                // negative would put an absurd number under someone's name.
                waitingCount: Int(clamping: row.waitingCount),
                unpostedWaitingCount: Int(clamping: row.unpostedWaitingCount),
                oldestWaitingMs: row.oldestWaitingMs,
                lastProgressMs: row.lastProgressMs,
                oversizedWaiting: row.oversizedWaiting,
                relayRejectStreak: row.relayRejectStreak,
                relayRejectedAtMs: row.relayRejectedAtMs,
                relayUnreachableStreak: row.relayUnreachableStreak,
                relayUnreachableAtMs: row.relayUnreachableAtMs
            )
        }

        let summaryRows: [PeerConnectionSummary] = (try? store.peerConnectionSummaries()) ?? []
        var summaries: [Data: [PeerConnectionSummary]] = [:]
        for row in summaryRows {
            summaries[row.userId, default: []].append(row)
        }

        let people: [ConnectionPerson] = contacts.map { contact in
            let rows = summaries[contact.userId] ?? []
            let latest = ConnectionActivityLogic.latestPeerStatus(rows)
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
                delivery: deliveryFacts[contact.userId] ?? PersonDeliveryFacts.none,
                latest: evidence,
                lastDeliveredMs: rows.compactMap { $0.lastDeliveredAtMs }.max() ?? 0
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

    /**
     The recent events inside one person's expansion.

     Deliberately not part of `load`. Five events per friend folded into the
     page reload would be one query per contact every four seconds -- bounded
     per query and unbounded in aggregate, which is the shape of cost this page
     is under orders to avoid. A reader opening one row is one query, once, for
     the row they opened.

     Runs off the main actor only, like every other store call here.
     */
    static func loadPersonEvents(userId: Data, name: String) -> [ConnectionActivityRow] {
        let store = AppStore.get()
        let events: [PeerConnectionEvent] = (try? store.peerConnectionEvents(
            userId: userId,
            limit: connectionPersonEventLimit
        )) ?? []
        return events.map { event in
            ConnectionActivityRow(
                name: name,
                evidence: ConnectionActivityLogic.evidence(of: event.kind),
                path: ConnectionDetailsLogic.observedPath(event.transport),
                atMs: event.occurredAtMs
            )
        }
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
 The page's refresh engine and its view state: what to reload, when, on which
 actor, and what the whole of it adds up to.

 Everything the page renders is derived here, once per change, and published as
 one finished `ConnectionDetailsState`. Phase 1 derived it inside the SwiftUI
 view body instead, which meant two FFI round trips marshalling the whole
 address book each way every time *any* observable moved -- including ones the
 page does not render -- and again for every unrelated redraw. That was recorded
 as owed work in #281 and is paid here, alongside the read model whose arrival
 is what made it worth doing.

 The live signals are mirrored into plain stored properties as they arrive
 rather than read back out of the observables, so a rebuild always sees exactly
 the values that were published and never a half-applied set.

 The refresh rules are the spec's, and they are not decoration. This screen
 reads the same store and the same change stream that has already contributed
 to a main-actor pileup during a mesh flood:

 - **Coalesce**: a burst of signals collapses into one reload
   (`StoreChangeCoalescer`, 500 ms).
 - **Never on main**: the load runs in a detached task and only the finished
   snapshot comes back to the main actor.
 - **Single flight**: one consumer loop, so at most one reload is in progress,
   and exactly one follow-up is owed for signals that arrive mid-reload.
 - **Bounded**: every query the load runs is limited.

 `stop()` tears all of it down -- tasks and observations both -- so navigating
 away ends every page-driven task and every subscription.
 */
@MainActor
final class ConnectionDetailsModel: ObservableObject {
    /// Everything the page renders, already interpreted by the core.
    @Published private(set) var state = ConnectionDetailsState.checking
    /// The person row whose expansion is open, by id rather than index so a
    /// reload that reorders the groups cannot swap which person is expanded
    /// under the reader.
    @Published private(set) var selectedPersonHex: String?
    /// The open row's recent events; nil while its bounded query is running.
    @Published private(set) var selectedPersonEvents: [ConnectionActivityRow]?
    /// Ticks while the page is visible, so relative times and the freshness
    /// label age on screen instead of freezing at the last store change. The
    /// renderer measures every time it prints against this one instant, so no
    /// two rows can disagree about "now".
    @Published private(set) var nowMs: Int64 = ConnectionClock.nowMs

    /// Bounds how long the health card may say `Checking`. Owned here because
    /// a mark that restarted on every render would make the bound unreachable.
    private let checkingClock = CheckingClock()

    private var snapshot = ConnectionStoreSnapshot.empty
    private var refreshing = false
    /// Set while the subscriptions are being attached. Each `@Published`
    /// publisher replays its current value the moment it is subscribed to, so
    /// without this the page would derive itself once per signal before it had
    /// finished wiring them up.
    private var rebuildsSuspended = false

    // Live signals, mirrored as they are published. Defaults are the honest
    // "nothing known yet" values; `start()` seeds them from the observables
    // before the first rebuild.
    private var ownUserId = Data()
    private var runtimeState: MeshRuntimeState = .stopped
    private var bluetoothAuthorizationBlocked = false
    private var bluetoothRadioState: CBManagerState = .unknown
    private var bluetoothAudioActive = false
    private var directPaths: [Data: DirectPath] = [:]
    private var relayHealth: RelayHealth = .noConfig
    private var presenceLastSeen: [Data: Int64] = [:]
    private var contactLastSeen: [Data: Int64] = [:]
    private var lanListening = false
    private var relayConfigured = false

    private let coalescer = StoreChangeCoalescer()
    private var requests: AsyncStream<Void>.Continuation?
    private var loopTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?
    private var clockTask: Task<Void, Never>?
    private var personEventsTask: Task<Void, Never>?
    private var cancellables: Set<AnyCancellable> = []
    private var pullWaiters: [CheckedContinuation<Void, Never>] = []

    /// Nonisolated so `@StateObject private var model = ConnectionDetailsModel()`
    /// needs no actor hop to build the view.
    nonisolated init() {}

    var isRunning: Bool { loopTask != nil }

    func start(ownUserId: Data) {
        self.ownUserId = ownUserId
        guard loopTask == nil else { return }
        // A previous run may have been torn down mid-window or mid-load.
        coalescer.reset()
        observeLiveSignals()

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
                self.rebuild()
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
        personEventsTask?.cancel()
        personEventsTask = nil
        requests?.finish()
        requests = nil
        // Every page-driven observation ends with the page, not just its
        // timers: a backgrounded diagnostics screen rebuilding its view state
        // on every mesh event earns nothing but battery.
        cancellables.removeAll()
        refreshing = false
        // Published, not just recorded: torn down inside a load, the last state
        // anybody saw carries a progress indicator, and it would still be
        // spinning over frozen rows when the page came back.
        rebuild()
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

    /// A saved Shore Pass may have been added or removed on another screen.
    /// Read on demand rather than per render: it changes only from there.
    func refreshRelayConfigured() {
        let configured = RelayConfigStore.load() != nil
        guard configured != relayConfigured else { return }
        relayConfigured = configured
        rebuild()
    }

    /**
     Open the detail sheet on one person, or close it with nil.

     The events behind it are read when a reader asks for them, on a background
     task, and only for the person they opened.
     */
    func selectPerson(_ userIdHex: String?) {
        guard selectedPersonHex != userIdHex else { return }
        selectedPersonHex = userIdHex
        selectedPersonEvents = nil
        personEventsTask?.cancel()
        personEventsTask = nil
        guard userIdHex != nil else { return }
        loadSelectedPersonEvents()
    }

    /// The person the detail sheet is open on, as the newest reload found them.
    var selectedPerson: ConnectionPersonRow? {
        guard let hex = selectedPersonHex else { return nil }
        let groups = [state.needsAttention, state.reachableNow, state.otherPeople]
        for group in groups {
            if let row = group.first(where: { $0.userIdHex == hex }) { return row }
        }
        return nil
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

    // MARK: - Live signals

    /**
     Mirror the observables this page reads, and rebuild when one moves.

     Each subscription stores the published value and rebuilds; nothing is read
     back off the observable, so there is no window in which a rebuild sees one
     new value beside an old one. The local Wi-Fi signal arrives already reduced
     to a deduplicated boolean (`LanListeningSignal`) -- the raw LAN snapshot
     changes on every peer and every sweep, and rebuilding this page at that
     rate for a flag that flips when the mesh starts is exactly the cost the
     performance rules forbid.
     */
    private func observeLiveSignals() {
        let runtime = MeshRuntimeStatus.shared
        let connectivity = MeshConnectivityStatus.shared
        let bluetooth = BluetoothAccess.shared
        let lan = LanListeningSignal.shared

        rebuildsSuspended = true
        // Seed from the current values, so the first rebuild is not waiting on
        // signals that may not move again for minutes.
        runtimeState = runtime.state
        bluetoothAudioActive = runtime.bluetoothAudioConnected
        bluetoothAuthorizationBlocked = bluetooth.isAuthorizationBlocked
        bluetoothRadioState = bluetooth.radioState
        directPaths = connectivity.directPaths
        relayHealth = connectivity.relay
        presenceLastSeen = connectivity.presenceLastSeen
        contactLastSeen = connectivity.contactLastSeen
        lanListening = lan.isListening
        relayConfigured = RelayConfigStore.load() != nil

        runtime.$state
            .sink { [weak self] value in
                guard let self = self else { return }
                self.runtimeState = value
                self.rebuild()
            }
            .store(in: &cancellables)
        runtime.$bluetoothAudioConnected
            .sink { [weak self] value in
                guard let self = self else { return }
                self.bluetoothAudioActive = value
                self.rebuild()
            }
            .store(in: &cancellables)
        bluetooth.$authorization
            .sink { [weak self] value in
                guard let self = self else { return }
                self.bluetoothAuthorizationBlocked = value == .denied || value == .restricted
                self.rebuild()
            }
            .store(in: &cancellables)
        bluetooth.$radioState
            .sink { [weak self] value in
                guard let self = self else { return }
                self.bluetoothRadioState = value
                self.rebuild()
            }
            .store(in: &cancellables)
        connectivity.$directPaths
            .sink { [weak self] value in
                guard let self = self else { return }
                self.directPaths = value
                self.rebuild()
            }
            .store(in: &cancellables)
        connectivity.$relay
            .sink { [weak self] value in
                guard let self = self else { return }
                self.relayHealth = value
                self.rebuild()
            }
            .store(in: &cancellables)
        connectivity.$presenceLastSeen
            .sink { [weak self] value in
                guard let self = self else { return }
                self.presenceLastSeen = value
                self.rebuild()
            }
            .store(in: &cancellables)
        connectivity.$contactLastSeen
            .sink { [weak self] value in
                guard let self = self else { return }
                self.contactLastSeen = value
                self.rebuild()
            }
            .store(in: &cancellables)
        lan.$isListening
            .sink { [weak self] value in
                guard let self = self else { return }
                self.lanListening = value
                self.rebuild()
            }
            .store(in: &cancellables)
        // The store-change signal the spec asks for. It fires per message and
        // per receipt, so at mesh-flood rates it arrives thousands of times a
        // minute -- which is exactly what the coalescer is for: each of these
        // costs one comparison, not one reload.
        ChatEvents.subject
            .sink { [weak self] _ in self?.signalStoreChanged() }
            .store(in: &cancellables)

        rebuildsSuspended = false
        rebuild()
    }

    /**
     Derive the whole view state from the mirrored signals and the last
     snapshot.

     The `CheckingClock` mark uses the same instant the classification is
     given: a mark stamped from a fresher clock than `nowMs` would look like it
     came from the future and resolve the bound instantly, so `Checking` would
     never be shown at all.
     */
    private func rebuild() {
        guard !rebuildsSuspended else { return }
        let availability = BluetoothAvailability.observed(
            authorizationBlocked: bluetoothAuthorizationBlocked,
            radioState: bluetoothRadioState
        )
        let coreRuntime = ConnectionInputs.runtime(runtimeState, bluetooth: availability)
        let coreRelay = ConnectionInputs.relay(relayHealth, configured: relayConfigured)
        let checkingSinceMs = checkingClock.mark(
            pending: connectionCheckPending(
                runtime: coreRuntime,
                bluetooth: ConnectionInputs.bluetooth(runtimeState, availability: availability),
                localWifi: ConnectionInputs.localWifi(runtimeState, listening: lanListening),
                relay: coreRelay
            ),
            nowMs: nowMs
        )
        let next = ConnectionDetailsLogic.buildState(
            runtimeState: runtimeState,
            bluetoothAvailability: availability,
            directPaths: directPaths,
            relayHealth: relayHealth,
            relayConfigured: relayConfigured,
            lanListening: lanListening,
            bluetoothAudioActive: bluetoothAudioActive,
            presenceLastSeen: presenceLastSeen,
            contactLastSeen: contactLastSeen,
            snapshot: snapshot,
            checkingSinceMs: checkingSinceMs,
            refreshing: refreshing,
            nowMs: nowMs
        )
        // A rebuild that changes nothing anybody can see is not a redraw.
        guard next != state else { return }
        state = next
    }

    // MARK: - Reload

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
        refreshing = true
        rebuild()
        let loadedAtMs = ConnectionClock.nowMs
        let ownUserId = self.ownUserId
        // The only place this page touches the store, and it is never the main
        // actor.
        let loaded = await Task.detached(priority: .userInitiated) {
            ConnectionSnapshotLoader.load(ownUserId: ownUserId, nowMs: loadedAtMs)
        }.value
        // The last good snapshot stayed on screen throughout; it is replaced
        // only once a whole new one exists.
        snapshot = loaded
        nowMs = ConnectionClock.nowMs
        refreshing = false
        rebuild()
        // An open sheet ages with the page: its five events are re-read after
        // each reload, so a new one appears there inside the same five seconds
        // the spec asks of the main list.
        loadSelectedPersonEvents()
        resumePullWaiters()
        if coalescer.onReloadFinished() { signalStoreChanged() }
    }

    /// Read the open sheet's events off the main actor, if a sheet is open.
    private func loadSelectedPersonEvents() {
        personEventsTask?.cancel()
        personEventsTask = nil
        // A page that has been torn down starts no new store work. `reload()`
        // can reach here after `stop()`: the detached load it is suspended on
        // is not cancelled by its parent, so it returns to a dead page and
        // runs on to this call, and `selectedPersonHex` is still set because
        // stopping does not close the sheet.
        guard loopTask != nil else { return }
        guard let hex = selectedPersonHex else { return }
        guard let person = snapshot.people.first(where: { $0.userIdHex == hex }) else {
            // Known to be open, but not in the snapshot: nothing to read, and
            // saying so beats a spinner that never resolves.
            selectedPersonEvents = []
            return
        }
        // Pulled out of the struct before the hop so only two Sendable values
        // cross into the detached task.
        let userId = person.userId
        let name = person.name
        personEventsTask = Task { [weak self] in
            let events = await Task.detached(priority: .userInitiated) {
                ConnectionSnapshotLoader.loadPersonEvents(userId: userId, name: name)
            }.value
            guard let self = self, !Task.isCancelled else { return }
            // The reader may have closed this row, or opened another, while
            // the query ran.
            guard self.selectedPersonHex == hex else { return }
            self.selectedPersonEvents = events
        }
    }

    private func resumePullWaiters() {
        let waiters = pullWaiters
        pullWaiters = []
        for waiter in waiters { waiter.resume() }
    }
}
