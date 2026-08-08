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

    // MARK: How to fix

    /**
     The How-to-fix explanation for a device-wide fault.

     Every one of these is written for someone who will not open a settings
     screen on their own, and each ends by saying what still works, because
     none of these faults stops delivery when the two phones are near each
     other.
     */
    static func howToFix(_ reason: CoreHealthReason) -> String? {
        switch reason {
        case .ownSetupRejected:
            return String(localized: "Shore Pass didn't accept this phone's saved setup. Open Shore Pass and set it up again, or check the setup against another phone in your family.")
        case .storageFull:
            return String(localized: "Your family's Shore Pass storage is full. Space frees up as your friends collect their messages, so this usually clears on its own. If it lasts more than a day, contact support.")
        case .passExpired:
            return String(localized: "Your Shore Pass has run out, so messages can't travel over the internet right now. Open Manage Shore Pass and renew it. Messages still reach your friends whenever you are near each other.")
        case .passSuspended:
            return String(localized: "Your Shore Pass has been turned off, so messages can't travel over the internet right now. Open Manage Shore Pass to see why and to turn it back on. Messages still reach your friends whenever you are near each other.")
        default:
            return nil
        }
    }

    /**
     The How-to-fix explanation for a fault stopping delivery to one friend.

     Every reason has one. A blocked row offers the control unconditionally, so
     a reason with nothing behind it would open an empty sheet -- which is
     worse than the silence it replaced.

     The order-sensitive one is the friend's rejected card: a card shared before
     the friend has fixed their own pass carries the same broken setup, so a
     reader who does these out of order fixes nothing and has to start again.
     The steps are numbered and the warning sits directly under them.
     */
    static func howToFix(_ reason: CoreDeliveryBlockedReason, name: String) -> String {
        switch reason {
        case .contactSetupRejected:
            return String(localized: "\(name)'s phone saved a Shore Pass setup that isn't accepting messages, so yours are waiting.\n\nDo these three things in this order:\n\n1. Ask \(name) to open Shore Pass on their own phone and get it working again. This has to happen first.\n2. After that is working, ask them to share their friend card with you again.\n3. Scan the new card they send you.\n\nThe order matters. A friend card shared before their Shore Pass is fixed carries the same setup, so the messages keep waiting and you have to start over.\n\nUntil then, your messages still reach \(name) whenever the two of you are near each other.")
        // The number a reader can act on is the one the composer enforces:
        // ATTACHMENT_MAX_BLOB_BYTES in core/src/content.rs, 180 KiB. The
        // sealed-envelope ceiling that produces this fault
        // (MAX_ENVELOPE_SEALED_BYTES, 512 KiB) is nearly three times larger
        // and unreachable through the normal send path, so quoting it would
        // teach the wrong limit and the advice would fail on a photo that
        // obeyed it. Keep this copy in step with core/src/content.rs.
        case .messageTooLarge:
            return String(localized: "Something you sent to \(name) is too big to travel. Photos and voice notes have to be under about 180 KB.\n\nOpen your conversation with \(name), delete the message that won't send, and send a smaller photo or a shorter voice note instead.")
        case .passExpired:
            return String(localized: "Your Shore Pass has run out, so messages can't travel over the internet right now. Open Manage Shore Pass and renew it. Messages still reach your friends whenever you are near each other.")
        case .passSuspended:
            return String(localized: "Your Shore Pass has been turned off, so messages can't travel over the internet right now. Open Manage Shore Pass to see why and to turn it back on. Messages still reach your friends whenever you are near each other.")
        case .storageFull:
            return String(localized: "Your family's Shore Pass storage is full. Space frees up as your friends collect their messages, so this usually clears on its own. If it lasts more than a day, contact support.")
        case .ownSetupRejected:
            return String(localized: "Shore Pass didn't accept this phone's saved setup. Open Shore Pass and set it up again, or check the setup against another phone in your family.")
        }
    }

    /// Only reachable if a new fault ships without its instructions. Says so
    /// plainly rather than opening an empty sheet.
    static func howToFixUnknown() -> String {
        String(localized: "CruiseMesh doesn't have step-by-step help for this one yet. Open Troubleshooting & diagnostics, share diagnostics, and contact support.")
    }

    /// Does this fault have a button on it, and does that button do something?
    static func offersManageShorePass(_ reason: CoreDeliveryBlockedReason) -> Bool {
        switch reason {
        case .passExpired, .passSuspended, .ownSetupRejected:
            return true
        // Nothing on the Shore Pass screen repairs a friend's card, an
        // oversized message, or a full mailbox, and a button that leads
        // somewhere useless costs a reader more than no button at all.
        case .contactSetupRejected, .storageFull, .messageTooLarge:
            return false
        }
    }

    static func offersManageShorePass(_ reason: CoreHealthReason) -> Bool {
        switch reason {
        case .passExpired, .passSuspended, .ownSetupRejected:
            return true
        default:
            return false
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

    /// Needs attention leads the page when it has anyone in it, and is omitted
    /// entirely when it does not.
    static func needsAttentionHeading(_ count: Int) -> String {
        String(localized: "Needs attention (\(count))")
    }

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

    /**
     One person's waiting work, as one sentence.

     The precedence is the core record's own: a blocking fault, then a stall on
     a working path, then where the work is going. The last of those is always
     true underneath the other two -- an expired pass stops the internet route,
     but the messages really will go the moment the friend is nearby -- which is
     why the fault is a *different* sentence beneath this one rather than a
     replacement for it.

     The age is appended when there is an honest one to append. `· 14 min` on a
     delayed row is the difference between a reader thinking something is stuck
     and knowing how stuck.
     */
    static func delivery(_ line: CoreDeliveryLine, nowMs: Int64) -> String {
        let count = Int(line.count)
        let headline: String
        if line.blockedReason != nil {
            headline = String(localized: "\(count) messages can't be sent")
        } else if line.delayed {
            headline = String(localized: "\(count) messages delayed")
        } else {
            headline = movement(line.state, count: count)
        }
        // Routine states carry no age on purpose: "3 messages will deliver when
        // you reconnect · 2 days" turns a promise into an accusation.
        if line.blockedReason == nil && !line.delayed { return headline }
        guard let age = waitingAge(line.oldestWaitingMs, nowMs: nowMs) else { return headline }
        return String(localized: "\(headline) · \(age)")
    }

    /// Where the work is going. None of these is a failure: a message waiting
    /// for a friend who is ashore is this app working.
    private static func movement(_ state: CoreDeliveryState, count: Int) -> String {
        switch state {
        case .sending:
            return String(localized: "Sending \(count) messages…")
        case .willDeliverWhenReconnected:
            return String(localized: "\(count) messages will deliver when you reconnect")
        case .waitingForInternet:
            return String(localized: "\(count) messages waiting for internet")
        }
    }

    /// How long the oldest waiting message has been waiting, or nil when there
    /// is no honest answer. A duration, never a moment: it must not read "ago"
    /// and must never become a calendar date.
    static func waitingAge(_ oldestWaitingMs: Int64, nowMs: Int64) -> String? {
        switch ConnectionTimes.waitingAge(sinceMs: oldestWaitingMs, nowMs: nowMs) {
        case .unknown:
            return nil
        case .minutes(let value):
            return String(localized: "\(value) min")
        case .hours(let value):
            return String(localized: "\(value) hours")
        case .days(let value):
            return String(localized: "\(value) days")
        }
    }

    /// Why an error row is an error row. The person's name is already above the
    /// line, so these do not repeat it.
    static func deliveryReason(_ reason: CoreDeliveryBlockedReason) -> String {
        switch reason {
        case .contactSetupRejected:
            return String(localized: "Their saved Shore Pass setup was rejected")
        case .passExpired:
            return String(localized: "Your Shore Pass has expired")
        case .passSuspended:
            return String(localized: "Your Shore Pass is suspended")
        case .storageFull:
            return String(localized: "Your family's Shore Pass storage is full")
        case .ownSetupRejected:
            return String(localized: "Shore Pass didn't accept this phone's saved setup")
        case .messageTooLarge:
            return String(localized: "A message is too large to send")
        }
    }

    // MARK: Person detail

    /// The core's routing answer, restated. Never re-derived here; see the spec.
    static func bestRoute(_ route: CorePersonRoute) -> String {
        switch route {
        case .directBluetooth: return String(localized: "Bluetooth")
        case .directLocalWifi: return String(localized: "Local Wi-Fi")
        case .shorePass: return String(localized: "Shore Pass")
        case .noneNow:
            return String(localized: "Nothing right now — messages travel when you next meet")
        }
    }

    static func personDetailNever() -> String { String(localized: "No record yet") }

    static func nothingWaiting() -> String { String(localized: "Nothing waiting") }

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

    /**
     Several facts as one announced sentence run.

     A person row has a variable number of them -- the delivery line and its
     reason come and go -- so the fixed-arity templates above cannot cover it,
     and joining with a literal `". "` here would hard-code English punctuation
     into the one path a screen-reader user depends on.
     */
    static func sentences(_ parts: [String]) -> String {
        let sentences = parts.map { String(localized: "\($0).") }
        guard var run = sentences.first else { return "" }
        for next in sentences.dropFirst() {
            run = String(localized: "\(run) \(next)")
        }
        return run
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

    @StateObject private var model = ConnectionDetailsModel()

    @Environment(\.dismiss) private var dismiss
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @Environment(\.scenePhase) private var scenePhase

    @State private var showClear = false
    @State private var otherPeopleExpanded = false
    @State private var activityExpanded = false
    @State private var showAllActivity = false
    @State private var troubleshootingExpanded = false
    /// A sheet rather than a scroll target. The spec forbids dropping a reader
    /// at the top of a long section to hunt for their answer, and a sheet is
    /// the only arrangement where "the explanation is on screen" is guaranteed
    /// rather than dependent on measured heights and where the list happened to
    /// be scrolled.
    @State private var howToFix: HowToFixTopic?
    @State private var showShorePass = false
    /// Set when `How to fix` asks for Shore Pass, consumed once that sheet has
    /// finished dismissing. See the `onDismiss` handoff below.
    @State private var shorePassAfterHowToFix = false

    @State private var diagnosticLogging = DiagnosticLogExport.isEnabled
    @State private var hasDiagnosticArchive = DiagnosticLogExport.hasArchive()
    @State private var shareFile: ShareableFile?
    @State private var supportMessage: String?

    var body: some View {
        // Derived once per change in `ConnectionDetailsModel`, not once per
        // body evaluation. This view reads a finished value and renders it.
        let state = model.state
        let times = ConnectionTimeContext(
            nowMs: model.nowMs,
            startOfTodayMs: ConnectionClock.startOfDayMs(model.nowMs)
        )
        return NavigationStack {
            List {
                healthSection(state)
                pathsSection(state.paths, times: times)
                // Needs attention comes first because it is the only group
                // anyone has to do something about; the spec's order, not a
                // layout preference.
                if !state.needsAttention.isEmpty {
                    peopleSection(
                        heading: ConnectionCopy.needsAttentionHeading(state.needsAttention.count),
                        rows: state.needsAttention,
                        times: times
                    )
                }
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
                model.start(ownUserId: appModel.identity.userId)
                // Two of the four probes behind this reach the store, and the
                // rule on this page is that no store query runs on the main
                // actor -- ever. During a flood the write lock is held by the
                // mesh queue, and blocking here would stall the whole app on
                // the one page rewritten to stop doing that.
                Task { await refreshCapturedDiagnostics() }
            }
            .onDisappear { model.stop() }
            // A backgrounded app never sends onDisappear, and a diagnostics
            // page polling from the background earns nothing but battery.
            .onChange(of: scenePhase) { phase in
                if phase == .active {
                    model.start(ownUserId: appModel.identity.userId)
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
                model.refreshRelayConfigured()
            }) {
                NavigationStack {
                    ShorePassView(initialCard: nil, appModel: appModel)
                }
            }
        }
        // On the navigation stack rather than the list: four sheet modifiers
        // stacked on one view is more than SwiftUI reliably presents, and these
        // two are the ones a reader reaches most.
        // The second presentation waits for the first to finish dismissing.
        // Asking for both in one state update is the classic swallowed
        // handoff: a presentation requested while the previous sheet is still
        // animating out is dropped, and a reader who tapped `Manage Shore
        // Pass` to repair an expired pass would land back on the page with
        // nothing opened and no way to fix it from here.
        .sheet(
            item: $howToFix,
            onDismiss: {
                guard shorePassAfterHowToFix else { return }
                shorePassAfterHowToFix = false
                showShorePass = true
            }
        ) { topic in
            HowToFixSheet(
                topic: topic,
                onManageShorePass: {
                    shorePassAfterHowToFix = true
                    howToFix = nil
                }
            )
        }
        .sheet(isPresented: personSheetBinding) {
            PersonDetailSheet(
                row: model.selectedPerson,
                events: model.selectedPersonEvents,
                times: times
            )
        }
    }

    /// Open while a person is selected; closing it clears the selection and
    /// cancels the events query behind it.
    private var personSheetBinding: Binding<Bool> {
        Binding(
            get: { model.selectedPersonHex != nil },
            set: { presented in if !presented { model.selectPerson(nil) } }
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
        // Qualified: `Group` unqualified is the core's chat-group record.
        SwiftUI.Group {
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
        let delivery = row.delivery.map { ConnectionCopy.delivery($0, nowMs: times.nowMs) }
        let blockedReason: CoreDeliveryBlockedReason? = row.delivery?.blockedReason
        let reason = blockedReason.map { ConnectionCopy.deliveryReason($0) }
        // One sentence per fact, in the order they are read on screen. The
        // delivery line has to be in here: the row replaces its children's
        // labels with this one, and anything left out is silent.
        // The badge name, not the mid-sentence one: the badge is what a sighted
        // reader sees on this row, and it is what TalkBack reads on the Android
        // row. Two screen readers saying different words for the same thing is
        // the kind of divergence this page was built to close.
        let statusPhrase = row.badge
            .map { ConnectionCopy.viaPath(status, ConnectionCopy.pathName($0)) } ?? status
        let label = ConnectionCopy.sentences(
            [row.name, statusPhrase] + [delivery, reason].compactMap { $0 }
        )
        VStack(alignment: .leading, spacing: 3) {
            // The whole row opens the person's detail, so it is a button; the
            // How-to-fix control below keeps its own label and its own tap
            // target rather than being swallowed by it.
            Button {
                model.selectPerson(row.userIdHex)
            } label: {
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
                        // Error color only when something has to change before
                        // the message can go; caution when a working path has
                        // stalled; otherwise the ordinary secondary color,
                        // because waiting is what this product does and the old
                        // page's red line under every friend is the bug being
                        // removed. Every one of these is paired with words that
                        // say the same thing.
                        Text(delivery)
                            .font(.subheadline)
                            .foregroundStyle(deliveryStyle(row.delivery))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let reason = reason {
                        Text(reason)
                            .font(.subheadline)
                            .foregroundStyle(Color.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            .frame(minHeight: 44)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(label))
            if let blocked = blockedReason {
                HStack {
                    Spacer(minLength: 0)
                    Button(ConnectionCopy.healthAction(.howToFix)) {
                        howToFix = .person(reason: blocked, name: row.name)
                    }
                    .buttonStyle(.borderless)
                    .frame(minHeight: 44)
                }
            }
        }
        .padding(.vertical, 2)
    }

    /// The color of a delivery line; see the comment at its call site.
    private func deliveryStyle(_ line: CoreDeliveryLine?) -> Color {
        if line?.blockedReason != nil { return .red }
        if line?.delayed == true { return .orange }
        return .secondary
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
            BluetoothAccess.shared.openSystemSettings()
        case .manageShorePass:
            showShorePass = true
        case .howToFix:
            // Never drop someone at the top of a long section to hunt for the
            // answer: put it in front of them.
            if let reason = reason { howToFix = .device(reason) }
        }
    }

    /// Everything captured, in one share sheet: the connection log, any crash
    /// reports MetricKit delivered for previous launches, the delivery timings
    /// CSV, and redacted stream-conflict summaries.
    ///
    /// The artifacts answer different questions -- what the radios did, why a
    /// launch died, whether messages actually arrived, whether a sender stream
    /// fork was quarantined, and what the protocol itself decided at each
    /// step -- and none is derivable from the others,
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
        if let url = ProtocolEventExport.writeJSONLFile() { urls.append(url) }
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
    /// Static and nonisolated because two of these reach the store, so it has
    /// to run off the main actor -- a `View` carries main-actor isolation onto
    /// its static members, and inheriting it here would put the store reads
    /// straight back where they were.
    nonisolated private static func hasAnythingCaptured() -> Bool {
        if DiagnosticLogExport.hasArchive() { return true }
        if !DiagnosticLogExport.metricKitFileURLs().isEmpty { return true }
        if FieldMetricsExport.hasCapturedMetrics() { return true }
        if ConflictDiagnosticsExport.hasCapturedConflicts() { return true }
        return ProtocolEventExport.hasCapturedEvents()
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
                // Both halves, or the next share rebuilds the file that was
                // just deleted from the ring that was not.
                try? AppStore.get().clearProtocolEvents()
                ProtocolEventExport.deleteExportedJSONL()
                // The last share left a zip holding copies of all of the above.
                DiagnosticsArchive.deleteArchives()
            }.value
            model.signalStoreChanged()
        }
    }
}

/**
 The How-to-fix content, on a sheet.

 A sheet rather than a scrolled-to paragraph inside Troubleshooting: the spec
 forbids dropping a reader at the top of a long section to find their own
 answer, and a sheet is the only arrangement that puts the explanation in front
 of them without depending on measured heights or scroll position.
 */
private struct HowToFixSheet: View {
    let topic: HowToFixTopic
    let onManageShorePass: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let explanation: String?
        let showManage: Bool
        switch topic {
        case .device(let reason):
            explanation = ConnectionCopy.howToFix(reason)
            showManage = ConnectionCopy.offersManageShorePass(reason)
        case .person(let reason, let name):
            explanation = ConnectionCopy.howToFix(reason, name: name)
            showManage = ConnectionCopy.offersManageShorePass(reason)
        }
        return NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    // A fault with no written remedy is not a blank sheet. The
                    // core only offers this control for faults that have one,
                    // so reaching here means a new reason arrived without its
                    // copy -- say so and point at the one thing that still
                    // helps.
                    Text(explanation ?? ConnectionCopy.howToFixUnknown())
                        .font(.body)
                        .fixedSize(horizontal: false, vertical: true)
                    if showManage {
                        Button(ConnectionCopy.healthAction(.manageShorePass)) {
                            onManageShorePass()
                        }
                        .buttonStyle(.borderedProminent)
                        .frame(maxWidth: .infinity, minHeight: 44)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(20)
            }
            .navigationTitle(ConnectionCopy.healthAction(.howToFix))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
    }
}

/**
 One person's detail sheet.

 Informational, by design and by specification -- CruiseMesh chooses the path,
 and offering a choice here would imply the choice matters. "Best route now" is
 the core's routing answer restated, never re-derived: a page that worked out
 reachability from "can I poll them" would report post-only friend cards as
 broken.
 */
private struct PersonDetailSheet: View {
    /// Nil when the open person left the address book mid-reload.
    let row: ConnectionPersonRow?
    /// Nil while the bounded events query is still running.
    let events: [ConnectionActivityRow]?
    let times: ConnectionTimeContext

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                if let row = row {
                    Section {
                        detailFact(
                            label: String(localized: "Best route now"),
                            value: ConnectionCopy.bestRoute(row.detail.bestRoute)
                        )
                        detailFact(
                            label: String(localized: "Last seen"),
                            value: ConnectionCopy.eventTime(row.detail.lastSeenMs, times: times)
                                ?? ConnectionCopy.personDetailNever()
                        )
                        detailFact(
                            label: String(localized: "Last received your message"),
                            value: ConnectionCopy.eventTime(
                                row.detail.lastDeliveredMs,
                                times: times
                            ) ?? ConnectionCopy.personDetailNever()
                        )
                        detailFact(
                            label: String(localized: "Waiting"),
                            value: row.delivery
                                .map { ConnectionCopy.delivery($0, nowMs: times.nowMs) }
                                ?? ConnectionCopy.nothingWaiting()
                        )
                    }
                    Section {
                        if let rows = events {
                            if rows.isEmpty {
                                Text("No connection events recorded yet.")
                                    .foregroundStyle(.secondary)
                            } else {
                                ForEach(Array(rows.enumerated()), id: \.offset) { _, event in
                                    if let line = ConnectionCopy.activityLine(event, times: times) {
                                        Text(line)
                                            .font(.subheadline)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                            }
                        } else {
                            // The query is still running. Saying "no events
                            // recorded" before it returns would be a claim the
                            // page cannot support, and for anyone with history
                            // it would be wrong.
                            ProgressView()
                                .accessibilityLabel(Text(ConnectionCopy.refreshing()))
                        }
                    } header: {
                        Text("Recent events")
                    }
                }
            }
            .navigationTitle(row?.name ?? String(localized: "Connection details"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
    }

    private func detailFact(label: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(minHeight: 44)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(ConnectionCopy.twoSentences(label, value)))
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
