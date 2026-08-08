import SwiftUI

/// The instants every rendered time on the page is measured against, passed
/// down together so no two rows can disagree about "now".
struct ConnectionTimeContext {
    let nowMs: Int64
    let startOfTodayMs: Int64
}

/**
 Every user-facing word on the Connection details page.

 Nothing here decides anything: it turns the core's enums and the view state's
 counts into copy. Keeping it in one place is what lets the page be reviewed
 for tone in one pass, and mirrors the `strings.xml` lookups at the bottom of
 ConnectionDetailsScreen.kt.

 House style applies throughout: sentence case, literal status copy, and no
 protocol jargon -- relay, envelope, hop, queue, and token never appear.
 `Shore Pass` is the product name for internet delivery and the only sanctioned
 way to refer to it.
 */
enum ConnectionCopy {

    // MARK: Health card

    static func healthTitle(_ state: CoreConnectionHealth) -> String {
        switch state {
        case .ready: return String(localized: "Working normally")
        case .limited: return String(localized: "Working, with limits")
        case .needsAttention: return String(localized: "Needs attention")
        case .checking: return String(localized: "Checking connections…")
        }
    }

    /**
     The evidence line: what is happening nearby, then the Shore Pass state.

     A stopped mesh gets the runtime half instead of a friend count, because
     "0 friends nearby" on a stopped service reads as an absence of friends
     rather than an absence of a running app.
     */
    static func healthEvidence(_ health: HealthCardState) -> String {
        let nearby: String
        if health.reason == CoreHealthReason.meshStopped {
            nearby = String(localized: "CruiseMesh is stopped")
        } else if health.nearbyFriendCount > 0 {
            nearby = friendsNearby(health.nearbyFriendCount)
        } else if health.bluetooth == CoreDirectPathState.off {
            nearby = String(localized: "Bluetooth is off")
        } else if health.bluetooth == CoreDirectPathState.starting {
            nearby = String(localized: "Starting up")
        } else {
            nearby = String(localized: "Listening for nearby friends")
        }
        let pass = relayEvidence(health.relay)
        return String(localized: "\(nearby) · \(pass)")
    }

    static func friendsNearby(_ count: Int) -> String {
        String(localized: "\(count) friends nearby")
    }

    /// The Shore Pass state, written to stand alone in the evidence line.
    static func relayEvidence(_ relay: CoreRelayPathState) -> String {
        switch relay {
        case .notSetUp: return String(localized: "Shore Pass not set up")
        case .checking: return String(localized: "Checking Shore Pass")
        case .connected: return String(localized: "Shore Pass connected")
        case .waitingForInternet: return String(localized: "Waiting for internet")
        case .unreachable: return String(localized: "Shore Pass unreachable")
        case .passExpired: return String(localized: "Shore Pass expired")
        case .passSuspended: return String(localized: "Shore Pass suspended")
        case .setupRejected: return String(localized: "Shore Pass setup rejected")
        case .storageFull: return String(localized: "Shore Pass storage full")
        case .syncingSlowed: return String(localized: "Shore Pass syncing slowed")
        }
    }

    static func healthAction(_ action: CoreHealthAction) -> String {
        switch action {
        case .startMesh: return String(localized: "Start mesh")
        case .turnOnBluetooth: return String(localized: "Turn on Bluetooth")
        case .manageShorePass: return String(localized: "Manage Shore Pass")
        case .howToFix: return String(localized: "How to fix")
        }
    }

    /// The How-to-fix explanation for the reasons this release can offer one
    /// for. Written for someone who will not open a settings screen on their
    /// own.
    static func howToFix(_ reason: CoreHealthReason) -> String? {
        switch reason {
        case .ownSetupRejected:
            return String(localized: "Shore Pass didn't accept this phone's saved setup. Open Shore Pass and set it up again, or check the setup against another phone in your family.")
        case .storageFull:
            return String(localized: "Your family's Shore Pass storage is full. Space frees up as your friends collect their messages, so this usually clears on its own. If it lasts more than a day, contact support.")
        default:
            return nil
        }
    }

    static func freshness(updatedAtMs: Int64, nowMs: Int64) -> String? {
        switch ConnectionTimes.freshness(updatedAtMs: updatedAtMs, nowMs: nowMs) {
        case .never:
            return nil
        case .justNow:
            return String(localized: "Updated just now")
        case .minutes(let value):
            return String(localized: "Updated \(value) min ago")
        case .hours(let value):
            return String(localized: "Updated \(value) hours ago")
        }
    }

    // MARK: Paths

    /// The path name as it appears on a Paths row and on a badge.
    static func pathName(_ badge: ConnectionPathBadge) -> String {
        switch badge {
        case .bluetooth: return String(localized: "Bluetooth")
        case .localWifi: return String(localized: "Local Wi-Fi")
        case .shorePass: return String(localized: "Shore Pass")
        }
    }

    /// The same path names written to sit mid-sentence ("… via local Wi-Fi").
    static func pathInSentence(_ badge: ConnectionPathBadge) -> String {
        switch badge {
        case .bluetooth: return String(localized: "Bluetooth")
        case .localWifi: return String(localized: "local Wi-Fi")
        case .shorePass: return String(localized: "Shore Pass")
        }
    }

    static func bluetoothPathState(_ paths: PathsCardState) -> String {
        if paths.bluetoothLinks > 0 { return activeLinks(paths.bluetoothLinks) }
        switch paths.bluetooth {
        case .off: return String(localized: "Off")
        case .starting: return String(localized: "Starting")
        case .available: return String(localized: "Listening")
        }
    }

    static func activeLinks(_ links: Int) -> String {
        if links == 0 { return String(localized: "No active connections") }
        return String(localized: "\(links) active connections")
    }

    /// The same states as the trailing text of the Shore Pass row, where the
    /// row already says "Shore Pass" on the left.
    static func relayPathState(_ relay: CoreRelayPathState) -> String {
        switch relay {
        case .notSetUp: return String(localized: "Not set up")
        case .checking: return String(localized: "Checking")
        case .connected: return String(localized: "Connected")
        case .waitingForInternet: return String(localized: "Waiting for internet")
        case .unreachable: return String(localized: "Unreachable")
        case .passExpired: return String(localized: "Pass expired")
        case .passSuspended: return String(localized: "Pass suspended")
        case .setupRejected: return String(localized: "Setup rejected")
        case .storageFull: return String(localized: "Storage full")
        case .syncingSlowed: return String(localized: "Syncing slowed")
        }
    }

    static func lastSyncedNote(_ paths: PathsCardState, times: ConnectionTimeContext) -> String? {
        // Only useful when the pass is set up at all; on a phone with no pass
        // it would be a date attached to nothing.
        if paths.relay == CoreRelayPathState.notSetUp { return nil }
        guard let time = eventTime(paths.relayLastSyncMs, times: times) else { return nil }
        return String(localized: "Last synced \(time)")
    }

    static func bluetoothAudioNote() -> String {
        String(localized: "Sharing the radio with Bluetooth audio.")
    }

    // MARK: People

    static func reachableNowHeading(_ count: Int) -> String {
        String(localized: "Reachable now (\(count))")
    }

    static func otherPeopleHeading(_ count: Int) -> String {
        String(localized: "Other people (\(count))")
    }

    static func showPeople(_ count: Int) -> String {
        String(localized: "Show \(count) people")
    }

    /// The status sentence under a person's name. The path is a badge beside
    /// the name, not part of the sentence.
    ///
    /// "Sent you a message" is THEIR message landing here; "Received your
    /// message" is a message THIS phone sent arriving at theirs. Swapping them
    /// is the bug this wording exists to prevent.
    static func personStatus(_ status: PersonStatus, times: ConnectionTimeContext) -> String {
        switch status {
        case .connectedNow:
            return String(localized: "Connected now")
        case .noHistory:
            return String(localized: "No connection history yet")
        case .seenOnline(let atMs):
            guard let time = eventTime(atMs, times: times) else {
                return String(localized: "Connected now")
            }
            return String(localized: "Seen online \(time)")
        case .history(let evidence, let atMs):
            guard let time = eventTime(atMs, times: times) else {
                // A recorded moment with no usable timestamp is not a date;
                // say what is actually known, which is nothing.
                return String(localized: "No connection history yet")
            }
            switch evidence {
            case .messageReceived: return String(localized: "Sent you a message \(time)")
            case .messageDelivered: return String(localized: "Received your message \(time)")
            case .presenceSeen: return String(localized: "Seen \(time)")
            case .connected: return String(localized: "Last connected \(time)")
            case .disconnected: return String(localized: "Last disconnected \(time)")
            }
        }
    }

    /// Waiting messages, in outcome terms. None of these is a failure: a
    /// message waiting for a friend who is ashore is this app working.
    static func delivery(_ line: DeliveryLine) -> String {
        switch line.kind {
        case .sending:
            return String(localized: "Sending \(line.count) messages…")
        case .willDeliverWhenReconnected:
            return String(localized: "\(line.count) messages will deliver when you reconnect")
        case .waitingForInternet:
            return String(localized: "\(line.count) messages waiting for internet")
        }
    }

    // MARK: Recent activity

    /// One activity line, or nil when the event carries no usable timestamp --
    /// which must never come out the other side as a date in 1970.
    static func activityLine(_ row: ConnectionActivityRow, times: ConnectionTimeContext) -> String? {
        guard let time = eventTime(row.atMs, times: times) else { return nil }
        let name = row.name ?? String(localized: "Friend")
        guard let observed = row.path else {
            // Another device carried it: we saw a hop to the phone in the
            // middle, never a path to this friend. Say what happened and stop
            // there rather than naming a radio they may be nowhere near.
            switch row.evidence {
            case .messageReceived: return String(localized: "\(name) sent you a message · \(time)")
            case .messageDelivered: return String(localized: "\(name) received your message · \(time)")
            case .presenceSeen: return String(localized: "\(name) was reachable · \(time)")
            case .connected: return String(localized: "\(name) connected · \(time)")
            case .disconnected: return String(localized: "\(name) disconnected · \(time)")
            }
        }
        let path = pathInSentence(observed)
        switch row.evidence {
        case .messageReceived: return String(localized: "\(name) sent you a message via \(path) · \(time)")
        case .messageDelivered: return String(localized: "\(name) received your message via \(path) · \(time)")
        case .presenceSeen: return String(localized: "\(name) was reachable via \(path) · \(time)")
        case .connected: return String(localized: "\(name) connected via \(path) · \(time)")
        case .disconnected: return String(localized: "\(name) disconnected via \(path) · \(time)")
        }
    }

    // MARK: Times

    /**
     A recorded moment as copy, or nil when there is no usable timestamp.

     Nil is the whole point: a zero or negative stamp must never come out the
     other side as a date in 1970.
     */
    static func eventTime(_ atMs: Int64, times: ConnectionTimeContext) -> String? {
        let bucket = ConnectionTimes.eventTime(
            atMs: atMs,
            nowMs: times.nowMs,
            startOfTodayMs: times.startOfTodayMs
        )
        let date = Date(timeIntervalSince1970: TimeInterval(atMs) / 1_000)
        switch bucket {
        case .unknown:
            return nil
        case .justNow:
            return String(localized: "just now")
        case .minutes(let value):
            return String(localized: "\(value) min ago")
        case .hours(let value):
            return String(localized: "\(value) hours ago")
        case .yesterday:
            let clock = date.formatted(date: .omitted, time: .shortened)
            return String(localized: "yesterday at \(clock)")
        case .older:
            let stamp = date.formatted(date: .numeric, time: .shortened)
            return String(localized: "on \(stamp)")
        }
    }

    // MARK: Screen-reader labels

    /// A row is announced as one sentence, so a name and its status are never
    /// read as two unrelated items.
    static func twoSentences(_ first: String, _ second: String) -> String {
        String(localized: "\(first). \(second).")
    }

    static func threeSentences(_ first: String, _ second: String, _ third: String) -> String {
        String(localized: "\(first). \(second). \(third).")
    }

    static func viaPath(_ status: String, _ path: String) -> String {
        String(localized: "\(status) via \(path)")
    }

    /// A collapsed section heading with its newest event time beside it.
    static func sectionWithDetail(_ title: String, _ detail: String) -> String {
        String(localized: "\(title) · \(detail)")
    }

    static func refreshing() -> String {
        String(localized: "Refreshing")
    }
}

/**
 The Connection details page.

 Reads state; it does not change it. Opening the page starts no scan, no
 advertising change, and no sync -- the single exception is pull-to-refresh,
 which the user performs deliberately and which asks for exactly one bounded
 sync pass through the existing `RelaySyncEvents` plumbing.

 Live signals (runtime, transports, relay health, presence) come straight off
 their observable objects and land on screen within a frame. Everything that
 needs the store -- people, waiting work, activity -- goes through
 `ConnectionDetailsModel`: coalesced, single-flight, bounded, and never read on
 the main actor.

 All interpretation lives in the core (`core/src/connection_health.rs`) and all
 copy lives in `ConnectionCopy` above, backed by `Localizable.xcstrings`. This
 file is the join between them. Mirrors ConnectionDetailsScreen.kt section for
 section.
 */
struct ConnectionDetailsView: View {
    @ObservedObject var appModel: AppModel

    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @ObservedObject private var lan = LanTransportDiagnostics.shared
    @ObservedObject private var bluetooth = BluetoothAccess.shared
    @StateObject private var model = ConnectionDetailsModel()

    @Environment(\.dismiss) private var dismiss
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @Environment(\.scenePhase) private var scenePhase

    @State private var showClear = false
    @State private var otherPeopleExpanded = false
    @State private var activityExpanded = false
    @State private var showAllActivity = false
    @State private var troubleshootingExpanded = false
    @State private var howToFixReason: CoreHealthReason?
    @State private var showShorePass = false
    /// Read once rather than on every render: a saved pass changes only from
    /// the Shore Pass screen, and this page must not re-decode it per frame.
    @State private var relayConfigured = RelayConfigStore.load() != nil

    @State private var diagnosticLogging = DiagnosticLogExport.isEnabled
    @State private var hasDiagnosticArchive = DiagnosticLogExport.hasArchive()
    @State private var shareFile: ShareableFile?
    @State private var supportMessage: String?

    var body: some View {
        let state = currentState()
        let times = ConnectionTimeContext(
            nowMs: model.nowMs,
            startOfTodayMs: ConnectionClock.startOfDayMs(model.nowMs)
        )
        return NavigationStack {
            List {
                healthSection(state)
                pathsSection(state.paths, times: times)
                if !state.reachableNow.isEmpty {
                    peopleSection(
                        heading: ConnectionCopy.reachableNowHeading(state.reachableNow.count),
                        rows: state.reachableNow,
                        times: times
                    )
                }
                if !state.otherPeople.isEmpty {
                    otherPeopleSection(state.otherPeople, times: times)
                }
                // Only once a snapshot has actually been read. "No friends
                // added yet" is a claim, and asserting it on the first frame of
                // every open -- before the background load has returned -- is a
                // false one for everybody who has friends.
                if state.updatedAtMs > 0 && !state.hasContacts {
                    Section {
                        Text("No friends added yet.")
                            .foregroundStyle(.secondary)
                    }
                }
                activitySection(state.activity, times: times)
                troubleshootingSection()
            }
            .navigationTitle("Connection details")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .refreshable { await model.refreshFromPull() }
            .onAppear {
                relayConfigured = RelayConfigStore.load() != nil
                model.start()
                // Two of the four probes behind this reach the store, and the
                // rule on this page is that no store query runs on the main
                // actor -- ever. During a flood the write lock is held by the
                // mesh queue, and blocking here would stall the whole app on
                // the one page rewritten to stop doing that.
                Task { await refreshCapturedDiagnostics() }
            }
            .onDisappear { model.stop() }
            // The store-change signal the spec asks for. It fires per message
            // and per receipt, so at mesh-flood rates it arrives thousands of
            // times a minute -- which is exactly what the coalescer is for:
            // each of these costs one comparison, not one reload.
            .onReceive(ChatEvents.subject) { _ in model.signalStoreChanged() }
            // A backgrounded app never sends onDisappear, and a diagnostics
            // page polling from the background earns nothing but battery.
            .onChange(of: scenePhase) { phase in
                if phase == .active {
                    model.start()
                } else {
                    model.stop()
                }
            }
            .confirmationDialog(
                "Clear connection history?",
                isPresented: $showClear,
                titleVisibility: .visible
            ) {
                Button("Clear history", role: .destructive) {
                    // A delete over the whole event table, plus the wait for a
                    // store lock the receive path also wants: not work for the
                    // actor that has to keep answering taps.
                    Task {
                        await Task.detached(priority: .userInitiated) {
                            try? AppStore.get().clearPeerConnectionHistory()
                        }.value
                        model.signalStoreChanged()
                    }
                }
            } message: {
                Text("This removes local connection events and per-person path summaries. Messages and friends are not affected.")
            }
            .sheet(item: $shareFile) { file in
                ActivityShareView(items: file.urls)
            }
            .sheet(isPresented: $showShorePass, onDismiss: {
                relayConfigured = RelayConfigStore.load() != nil
            }) {
                NavigationStack {
                    ShorePassView(initialCard: nil, appModel: appModel)
                }
            }
        }
    }

    // MARK: - View state

    /**
     Live signals plus the last store snapshot, interpreted by the core.

     The `CheckingClock` mark uses the same instant the classification is
     given: a mark stamped from a fresher clock than `nowMs` would look like it
     came from the future and resolve the bound instantly, so `Checking` would
     never be shown at all.
     */
    private func currentState() -> ConnectionDetailsState {
        let availability = BluetoothAvailability.observed(
            authorizationBlocked: bluetooth.isAuthorizationBlocked,
            radioState: bluetooth.radioState
        )
        let coreRuntime = ConnectionInputs.runtime(runtime.state, bluetooth: availability)
        let coreRelay = ConnectionInputs.relay(connectivity.relay, configured: relayConfigured)
        // Only whether a listening socket exists. The endpoint itself never
        // reaches the view state, let alone the screen.
        let lanListening = lan.snapshot.localEndpoint != nil
        let checkingSinceMs = model.checkingClock.mark(
            pending: connectionCheckPending(
                runtime: coreRuntime,
                bluetooth: ConnectionInputs.bluetooth(
                    runtime.state,
                    availability: availability
                ),
                localWifi: ConnectionInputs.localWifi(runtime.state, listening: lanListening),
                relay: coreRelay
            ),
            nowMs: model.nowMs
        )
        return ConnectionDetailsLogic.buildState(
            runtimeState: runtime.state,
            bluetoothAvailability: availability,
            directPaths: connectivity.directPaths,
            relayHealth: connectivity.relay,
            relayConfigured: relayConfigured,
            lanListening: lanListening,
            bluetoothAudioActive: runtime.bluetoothAudioConnected,
            staleRelayContacts: connectivity.staleRelayContacts,
            presenceLastSeen: connectivity.presenceLastSeen,
            contactLastSeen: connectivity.contactLastSeen,
            snapshot: model.snapshot,
            checkingSinceMs: checkingSinceMs,
            refreshing: model.isRefreshing,
            nowMs: model.nowMs
        )
    }

    // MARK: - Health

    @ViewBuilder
    private func healthSection(_ state: ConnectionDetailsState) -> some View {
        let health = state.health
        let title = ConnectionCopy.healthTitle(health.state)
        let evidence = ConnectionCopy.healthEvidence(health)
        Section {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 10) {
                    healthIcon(health.state)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(title)
                            .font(.headline)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(evidence)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                // Scoped to the two lines it describes: merging the whole card
                // would swallow the action button's own label, which is the
                // one thing on the card a person can act on.
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(Text(ConnectionCopy.twoSentences(title, evidence)))
                freshnessLine(state)
                if let action = health.action {
                    Button(ConnectionCopy.healthAction(action)) {
                        perform(action, reason: health.reason)
                    }
                    .buttonStyle(.borderedProminent)
                    .frame(minHeight: 44)
                }
            }
            .padding(.vertical, 4)
        }
    }

    @ViewBuilder
    private func healthIcon(_ state: CoreConnectionHealth) -> some View {
        switch state {
        case .checking:
            // The title beside it already says "Checking connections…", and
            // the card is announced as one label, so the spinner itself stays
            // silent rather than repeating it.
            ProgressView()
                .accessibilityHidden(true)
        case .ready:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .accessibilityHidden(true)
        case .limited:
            Image(systemName: "info.circle.fill")
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
        case .needsAttention:
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .accessibilityHidden(true)
        }
    }

    @ViewBuilder
    private func freshnessLine(_ state: ConnectionDetailsState) -> some View {
        if let label = ConnectionCopy.freshness(
            updatedAtMs: state.updatedAtMs,
            nowMs: model.nowMs
        ) {
            HStack(spacing: 6) {
                Text(label)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                if state.refreshing {
                    ProgressView()
                        .scaleEffect(0.7)
                        .accessibilityLabel(Text(ConnectionCopy.refreshing()))
                }
            }
        }
    }

    // MARK: - Paths

    private func pathsSection(
        _ paths: PathsCardState,
        times: ConnectionTimeContext
    ) -> some View {
        Section {
            pathRow(
                systemImage: "dot.radiowaves.left.and.right",
                name: ConnectionCopy.pathName(.bluetooth),
                state: ConnectionCopy.bluetoothPathState(paths),
                note: paths.bluetoothAudioActive ? ConnectionCopy.bluetoothAudioNote() : nil
            )
            pathRow(
                systemImage: "wifi",
                name: ConnectionCopy.pathName(.localWifi),
                state: ConnectionCopy.activeLinks(paths.localWifiLinks),
                note: nil
            )
            pathRow(
                systemImage: "antenna.radiowaves.left.and.right",
                name: ConnectionCopy.pathName(.shorePass),
                state: ConnectionCopy.relayPathState(paths.relay),
                note: ConnectionCopy.lastSyncedNote(paths, times: times)
            )
        } header: {
            Text("Paths")
        } footer: {
            Text("CruiseMesh chooses the best available path automatically. A message may arrive by Bluetooth, local Wi-Fi, or Shore Pass.")
        }
    }

    @ViewBuilder
    private func pathRow(
        systemImage: String,
        name: String,
        state: String,
        note: String?
    ) -> some View {
        let label = ConnectionCopy.twoSentences(name, state)
        Group {
            // Side by side there is not enough width for both halves at
            // accessibility text sizes, and a name column narrower than its
            // longest word wraps one letter per line. Stacking is the honest
            // answer: nothing truncates and nothing has to fit.
            if dynamicTypeSize.isAccessibilitySize {
                VStack(alignment: .leading, spacing: 3) {
                    Label { Text(name) } icon: { Image(systemName: systemImage) }
                    Text(state).foregroundStyle(.secondary)
                    pathRowNote(note)
                }
            } else {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        Label { Text(name) } icon: { Image(systemName: systemImage) }
                        pathRowNote(note)
                    }
                    Spacer(minLength: 12)
                    Text(state)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.trailing)
                }
            }
        }
        .frame(minHeight: 44)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(label))
    }

    @ViewBuilder
    private func pathRowNote(_ note: String?) -> some View {
        if let note = note {
            Text(note)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - People

    private func peopleSection(
        heading: String,
        rows: [ConnectionPersonRow],
        times: ConnectionTimeContext
    ) -> some View {
        Section {
            ForEach(rows, id: \.userIdHex) { row in
                personRow(row, times: times)
            }
        } header: {
            Text(heading)
        }
    }

    @ViewBuilder
    private func otherPeopleSection(
        _ rows: [ConnectionPersonRow],
        times: ConnectionTimeContext
    ) -> some View {
        let collapsed = rows.count > connectionOtherPeopleCollapseAt && !otherPeopleExpanded
        let shown = collapsed ? Array(rows.prefix(connectionOtherPeopleCollapseAt)) : rows
        let hidden = rows.count - connectionOtherPeopleCollapseAt
        Section {
            ForEach(shown, id: \.userIdHex) { row in
                personRow(row, times: times)
            }
            if rows.count > connectionOtherPeopleCollapseAt {
                if collapsed {
                    Button(ConnectionCopy.showPeople(hidden)) { otherPeopleExpanded = true }
                        .frame(minHeight: 44)
                } else {
                    Button("Show less") { otherPeopleExpanded = false }
                        .frame(minHeight: 44)
                }
            }
        } header: {
            Text(ConnectionCopy.otherPeopleHeading(rows.count))
        }
    }

    @ViewBuilder
    private func personRow(_ row: ConnectionPersonRow, times: ConnectionTimeContext) -> some View {
        let status = ConnectionCopy.personStatus(row.status, times: times)
        let badge = row.badge.map { ConnectionCopy.pathName($0) }
        let delivery = row.delivery.map { ConnectionCopy.delivery($0) }
        // One sentence per fact, in the order they are read on screen. The
        // delivery line has to be in here: the row replaces its children's
        // labels with this one, and anything left out is silent.
        // The badge name, not the mid-sentence one: the badge is what a sighted
        // reader sees on this row, and it is what TalkBack reads on the Android
        // row. Two screen readers saying different words for the same thing is
        // the kind of divergence this page was built to close.
        let statusPhrase = row.badge
            .map { ConnectionCopy.viaPath(status, ConnectionCopy.pathName($0)) } ?? status
        let label = delivery
            .map { ConnectionCopy.threeSentences(row.name, statusPhrase, $0) }
            ?? ConnectionCopy.twoSentences(row.name, statusPhrase)
        VStack(alignment: .leading, spacing: 3) {
            if dynamicTypeSize.isAccessibilitySize {
                Text(row.name)
                    .fixedSize(horizontal: false, vertical: true)
                if let badge = badge {
                    PathBadgeLabel(text: badge)
                }
            } else {
                HStack(alignment: .firstTextBaseline) {
                    Text(row.name)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 8)
                    if let badge = badge {
                        PathBadgeLabel(text: badge)
                    }
                }
            }
            Text(status)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let delivery = delivery {
                // Neutral, always. Waiting is what this product does; the old
                // page's red line under every friend is the bug being removed.
                Text(delivery)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.vertical, 2)
        .frame(minHeight: 44)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(label))
    }

    // MARK: - Recent activity

    @ViewBuilder
    private func activitySection(
        _ activity: [ConnectionActivityRow],
        times: ConnectionTimeContext
    ) -> some View {
        let shown = showAllActivity
            ? activity
            : Array(activity.prefix(connectionActivityPreviewCount))
        Section {
            DisclosureGroup(isExpanded: $activityExpanded) {
                if activity.isEmpty {
                    Text("Connection activity will appear here as CruiseMesh reaches your friends.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(shown.enumerated()), id: \.offset) { _, row in
                        if let line = ConnectionCopy.activityLine(row, times: times) {
                            Text(line)
                                .font(.subheadline)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    if activity.count > connectionActivityPreviewCount {
                        if showAllActivity {
                            Button("Show less") { showAllActivity = false }
                                .frame(minHeight: 44)
                        } else {
                            Button("Show all activity") { showAllActivity = true }
                                .frame(minHeight: 44)
                        }
                    }
                }
            } label: {
                Text(activityHeading(activity, times: times))
                    .frame(minHeight: 44)
            }
        }
    }

    /// The collapsed Recent activity row.
    ///
    /// Collapsed, the newest event time is the only signal that anything
    /// happened at all; without it the row gives a reader no reason to open it.
    /// A row whose timestamp is zero or unreadable contributes nothing rather
    /// than rendering as a date.
    private func activityHeading(
        _ activity: [ConnectionActivityRow],
        times: ConnectionTimeContext
    ) -> String {
        let title = String(localized: "Recent activity")
        guard let newest = activity.first,
              let when = ConnectionCopy.eventTime(newest.atMs, times: times)
        else { return title }
        return ConnectionCopy.sectionWithDetail(title, when)
    }

    // MARK: - Troubleshooting and diagnostics

    @ViewBuilder
    private func troubleshootingSection() -> some View {
        Section {
            DisclosureGroup(isExpanded: $troubleshootingExpanded) {
                if let reason = howToFixReason, let text = ConnectionCopy.howToFix(reason) {
                    Text(text)
                        .font(.subheadline)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Toggle("Diagnostic logging", isOn: $diagnosticLogging)
                    .onChange(of: diagnosticLogging) { enabled in
                        DiagnosticLogExport.setEnabled(enabled)
                        supportMessage = enabled
                            ? String(localized: "Diagnostic logging is on. Reproduce the problem, then return here to share it.")
                            : String(localized: "Diagnostic logging is off. What was already captured is kept until you delete it.")
                    }
                Text("Turn this on before testing to keep the connection log across app restarts. Delivery timings are kept either way. Message content is never recorded.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button {
                    // One button, everything captured. Asking a family member
                    // to send "diagnostics" and having them come back with only
                    // half of what is needed costs a round trip that, on a
                    // ship, can take a day -- so the log, any crash reports,
                    // and the delivery timings all ride the same share sheet.
                    shareEverything()
                } label: {
                    Label("Share diagnostics", systemImage: "ladybug")
                }
                .frame(minHeight: 44)
                Button(role: .destructive) {
                    deleteEverythingCaptured()
                    supportMessage = String(localized: "Captured diagnostics deleted.")
                } label: {
                    Label("Delete captured diagnostics", systemImage: "trash")
                }
                .frame(minHeight: 44)
                .disabled(!hasDiagnosticArchive)
                Button("Clear connection history", role: .destructive) {
                    showClear = true
                }
                .frame(minHeight: 44)
                Text("Diagnostics contain friend identity, path type, event type, time, hashed chat tags, and delivery timings. They never contain message content, relay tokens, IP addresses, or Wi-Fi names.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let supportMessage = supportMessage {
                    Text(supportMessage)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } label: {
                Text("Troubleshooting & diagnostics")
                    .frame(minHeight: 44)
            }
        }
    }

    // MARK: - Actions

    private func perform(_ action: CoreHealthAction, reason: CoreHealthReason?) {
        switch action {
        case .startMesh:
            appModel.startMesh()
        case .turnOnBluetooth:
            // iOS has no "turn Bluetooth on" API; Settings is the only place a
            // person can do it, and this is the same route the home-screen
            // banner already takes.
            bluetooth.openSystemSettings()
        case .manageShorePass:
            showShorePass = true
        case .howToFix:
            // Never drop someone at the top of a long section to hunt for the
            // answer: expand it *and* name the reason inside it.
            howToFixReason = reason
            troubleshootingExpanded = true
        }
    }

    /// Everything captured, in one share sheet: the connection log, any crash
    /// reports MetricKit delivered for previous launches, the delivery timings
    /// CSV, and redacted stream-conflict summaries.
    ///
    /// The artifacts answer different questions -- what the radios did, why a
    /// launch died, whether messages actually arrived, and whether a sender
    /// stream fork was quarantined -- and none is derivable from the others,
    /// so splitting them across buttons only meant getting a partial answer
    /// from whoever tapped the obvious one.
    ///
    /// They go as a single zip rather than as several attachments -- see
    /// `DiagnosticsArchive` for how a list of files loses some of them.
    private func shareEverything() {
        var urls: [URL] = []
        if let url = DiagnosticLogExport.writeLogFile() { urls.append(url) }
        urls.append(contentsOf: DiagnosticLogExport.metricKitFileURLs())
        if let url = FieldMetricsExport.writeCSVFile() { urls.append(url) }
        if let url = ConflictDiagnosticsExport.writeCSVFile() { urls.append(url) }
        hasDiagnosticArchive = !urls.isEmpty
        if urls.isEmpty {
            supportMessage = String(localized: "No diagnostics captured yet.")
            return
        }
        // Zipping is a disk write and can fail -- a full device, most likely.
        // Sending the loose files then beats telling someone who has captured
        // diagnostics that they have none.
        let archive = DiagnosticsArchive.write(files: urls, name: DiagnosticsArchive.todaysName())
        shareFile = ShareableFile(urls: archive.map { [$0] } ?? urls)
    }

    /// Answers `hasAnythingCaptured` off the main actor and posts the result.
    @MainActor
    private func refreshCapturedDiagnostics() async {
        let captured = await Task.detached(priority: .utility) {
            ConnectionDetailsView.hasAnythingCaptured()
        }.value
        hasDiagnosticArchive = captured
    }

    /// Whether the delete button has anything to act on.
    ///
    /// Has to count everything `shareEverything` sends, or the two buttons
    /// disagree: a tester whose app crashed but who never turned diagnostic
    /// logging on would find delete greyed out while crash payloads sat on
    /// disk, share them, then be told they were deleted when they were not.
    /// Delivery metrics are captured unconditionally, and MetricKit collection
    /// is not gated by the logging switch either.
    ///
    /// Static because two of these reach the store, so it has to be callable
    /// from a detached task without dragging a view's worth of observed
    /// objects across with it.
    private static func hasAnythingCaptured() -> Bool {
        if DiagnosticLogExport.hasArchive() { return true }
        if !DiagnosticLogExport.metricKitFileURLs().isEmpty { return true }
        if FieldMetricsExport.hasCapturedMetrics() { return true }
        return ConflictDiagnosticsExport.hasCapturedConflicts()
    }

    /// Erases everything `shareEverything` would send. Anything left behind
    /// here becomes a lie the next share tells.
    ///
    /// The button updates first and the work happens off the main actor: two
    /// table-wide deletes and a handful of file removals, each waiting on a
    /// store lock the receive path also wants.
    @MainActor
    private func deleteEverythingCaptured() {
        hasDiagnosticArchive = false
        Task {
            await Task.detached(priority: .userInitiated) {
                DiagnosticLogExport.deleteArchive()
                for url in DiagnosticLogExport.metricKitFileURLs() {
                    try? FileManager.default.removeItem(at: url)
                }
                try? AppStore.get().clearDeliveryMetrics()
                FieldMetricsExport.deleteExportedCSV()
                try? AppStore.get().clearMessageConflicts()
                ConflictDiagnosticsExport.deleteExportedCSV()
                // The last share left a zip holding copies of all of the above.
                DiagnosticsArchive.deleteArchives()
            }.value
            model.signalStoreChanged()
        }
    }
}

/// A small outlined badge naming the path a row was reached on.
private struct PathBadgeLabel: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(Color.secondary.opacity(0.5), lineWidth: 1)
            )
    }
}
