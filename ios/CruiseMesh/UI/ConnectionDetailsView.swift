import SwiftUI

struct ConnectionDetailsView: View {
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var contacts: [Contact] = []
    @State private var summaries: [PeerConnectionSummary] = []
    @State private var events: [PeerConnectionEvent] = []
    @State private var queueDepths: [Data: UInt64] = [:]
    @State private var showClear = false
    @State private var showAllActivity = false
    @State private var diagnosticLogging = DiagnosticLogExport.isEnabled
    @State private var hasDiagnosticArchive = DiagnosticLogExport.hasArchive()
    @State private var shareFile: ShareableFile?
    @State private var supportMessage: String?

    var body: some View {
        NavigationStack {
            List {
                Section("Overview") {
                    LabeledContent("CruiseMesh", value: runtime.pillText)
                    LabeledContent(
                        "Bluetooth",
                        value: bluetoothCount == 0
                            ? "Listening"
                            : "\(bluetoothCount) active"
                    )
                    LabeledContent(
                        "Local Wi-Fi",
                        value: localWifiCount == 0
                            ? "No active links"
                            : "\(localWifiCount) active"
                    )
                    LabeledContent("Shore Pass", value: relayLabel)
                    Text("CruiseMesh chooses the best available path automatically. A message may arrive by Bluetooth, local Wi-Fi, or Shore Pass.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("People") {
                    if contacts.isEmpty {
                        Text("No friends added yet.")
                    } else {
                        ForEach(contacts, id: \.userId) { contact in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(coreContactDisplayName(contact: contact))
                                Text(personStatus(contact))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                if let queued = queueDepths[contact.userId], queued > 0 {
                                    Text("Pending relay upload: \(queued)")
                                        .font(.caption)
                                        .foregroundStyle(.red)
                                }
                            }
                        }
                    }
                }

                Section("Recent activity") {
                    if events.isEmpty {
                        Text("Connection activity will appear here as CruiseMesh reaches your friends.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(Array(visibleEvents.enumerated()), id: \.offset) { _, event in
                            Text(eventText(event)).font(.caption)
                        }
                        if events.count > Self.recentActivityPreviewCount {
                            Button(
                                showAllActivity
                                    ? "Show less"
                                    : "Show \(events.count) recent entries"
                            ) {
                                showAllActivity.toggle()
                            }
                        }
                    }
                }

                Section("Support") {
                    Toggle("Diagnostic logging", isOn: $diagnosticLogging)
                        .onChange(of: diagnosticLogging) {
                            DiagnosticLogExport.setEnabled($0)
                            supportMessage = $0
                                ? String(localized: "Diagnostic logging is on. Reproduce the problem, then return here to share it.")
                                : String(localized: "Diagnostic logging is off. What was already captured is kept until you delete it.")
                        }
                    Text("Turn this on before testing to keep the connection log across app restarts. Delivery timings are kept either way. Message content is never recorded.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button {
                        // One button, everything captured. Asking a family
                        // member to send "diagnostics" and having them come
                        // back with only half of what is needed costs a round
                        // trip that, on a ship, can take a day -- so the log,
                        // any crash reports, and the delivery timings all ride
                        // the same share sheet.
                        shareEverything()
                    } label: {
                        Label("Share diagnostics", systemImage: "ladybug")
                    }
                    Button(role: .destructive) {
                        deleteEverythingCaptured()
                        supportMessage = String(localized: "Captured diagnostics deleted.")
                    } label: {
                        Label("Delete captured diagnostics", systemImage: "trash")
                    }
                    .disabled(!hasDiagnosticArchive)
                    Button("Clear connection history", role: .destructive) {
                        showClear = true
                    }
                    Text("Diagnostics contain friend identity, path type, event type, time, hashed chat tags, and delivery timings. They never contain message content, relay tokens, IP addresses, or Wi-Fi names.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let supportMessage {
                        Text(supportMessage).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Connection details")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear(perform: reload)
            .confirmationDialog(
                "Clear connection history?",
                isPresented: $showClear,
                titleVisibility: .visible
            ) {
                Button("Clear history", role: .destructive) {
                    try? AppStore.get().clearPeerConnectionHistory()
                    reload()
                }
            } message: {
                Text("This removes local connection events and per-person path summaries. Messages and friends are not affected.")
            }
            .sheet(item: $shareFile) { file in
                ActivityShareView(items: file.urls)
            }
        }
    }

    private var relayLabel: String {
        guard RelayConfigStore.load() != nil else { return "Not configured" }
        switch connectivity.relay {
        case .noConfig: return RelayConfigStore.load() == nil ? "Not configured" : "Checking setup"
        case .checking: return "Checking setup"
        case .noInternet: return "Waiting for internet"
        case .ok: return "Connected"
        case .failing: return "Unreachable"
        case .expired: return "Pass expired"
        case .suspended: return "Pass suspended"
        case .tokenRejected: return "Setup rejected"
        case .quotaFull: return String(localized: "Storage full")
        case .messageTooLarge: return String(localized: "Message too large")
        case .rateLimited: return String(localized: "Syncing slowed")
        }
    }

    private var visibleEvents: ArraySlice<PeerConnectionEvent> {
        events.prefix(showAllActivity ? events.count : Self.recentActivityPreviewCount)
    }

    private static let recentActivityPreviewCount = 10

    private var bluetoothCount: Int {
        let contactIds = Set(contacts.map(\.userId))
        return connectivity.directPaths.filter {
            contactIds.contains($0.key) && $0.value == .bluetooth
        }.count
    }

    private var localWifiCount: Int {
        let contactIds = Set(contacts.map(\.userId))
        return connectivity.directPaths.filter {
            contactIds.contains($0.key) && $0.value == .localWifi
        }.count
    }

    private func reload() {
        let store = AppStore.get()
        contacts = (try? store.listContacts()) ?? []
        summaries = (try? store.peerConnectionSummaries()) ?? []
        events = (try? store.peerConnectionEvents(userId: nil, limit: 50)) ?? []
        
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        let depthList = (try? store.pendingRelayOutboundDepthByRecipient(nowMs: nowMs)) ?? []
        var depths: [Data: UInt64] = [:]
        for d in depthList {
            depths[d.recipientUserId] = d.queued
        }
        queueDepths = depths
        
        hasDiagnosticArchive = hasAnythingCaptured()
    }

    /// Everything captured, in one share sheet: the connection log, any crash
    /// reports MetricKit delivered for previous launches, and the delivery
    /// timings CSV.
    ///
    /// The three answer different questions -- what the radios did, why a
    /// launch died, and whether messages actually arrived -- and none is
    /// derivable from the others, so splitting them across buttons only meant
    /// getting a partial answer from whoever tapped the obvious one.
    ///
    /// They go as a single zip rather than as several attachments -- see
    /// `DiagnosticsArchive` for how a list of files loses some of them.
    private func shareEverything() {
        var urls: [URL] = []
        if let url = DiagnosticLogExport.writeLogFile() { urls.append(url) }
        urls.append(contentsOf: DiagnosticLogExport.metricKitFileURLs())
        if let url = FieldMetricsExport.writeCSVFile() { urls.append(url) }
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

    /// Whether the delete button has anything to act on.
    ///
    /// Has to count everything `shareEverything` sends, or the two buttons
    /// disagree: a tester whose app crashed but who never turned diagnostic
    /// logging on would find delete greyed out while crash payloads sat on
    /// disk, share them, then be told they were deleted when they were not.
    /// Delivery metrics are captured unconditionally, and MetricKit collection
    /// is not gated by the logging switch either.
    private func hasAnythingCaptured() -> Bool {
        if DiagnosticLogExport.hasArchive() { return true }
        if !DiagnosticLogExport.metricKitFileURLs().isEmpty { return true }
        return FieldMetricsExport.hasCapturedMetrics()
    }

    /// Erases everything `shareEverything` would send. Anything left behind
    /// here becomes a lie the next share tells.
    private func deleteEverythingCaptured() {
        DiagnosticLogExport.deleteArchive()
        for url in DiagnosticLogExport.metricKitFileURLs() {
            try? FileManager.default.removeItem(at: url)
        }
        try? AppStore.get().clearDeliveryMetrics()
        FieldMetricsExport.deleteExportedCSV()
        // The last share left a zip holding copies of all of the above.
        DiagnosticsArchive.deleteArchives()
        hasDiagnosticArchive = false
    }

    private func personStatus(_ contact: Contact) -> String {
        switch connectivity.directPaths[contact.userId] {
        case .localWifi:
            return String(localized: "Connected now via local Wi-Fi")
        case .bluetooth:
            return String(localized: "Connected now via Bluetooth")
        case nil:
            break
        }
        let rows = summaries.filter { $0.userId == contact.userId }
        // A zero timestamp is not evidence. `recordPeerConnectionEvent` only
        // rejects negative values, and an arrival is stamped with raw wall
        // clock, so a phone with an unset clock can persist 0 -- which would
        // otherwise render as a confident "1 Jan 1970".
        guard let latest = ConnectionActivityLogic.latestPeerStatus(rows), latest.atMs > 0 else {
            return String(localized: "No connection history yet")
        }
        let time = formatTime(latest.atMs)
        guard let path = transportLabel(latest.transport) else {
            // Another device carried it: we saw a hop to the phone in the
            // middle, never a path to this friend. Say what happened and stop
            // there rather than naming a radio they may be nowhere near.
            switch latest.evidence {
            case .messageReceived:
                return String(localized: "Sent you a message · \(time)")
            case .messageDelivered:
                return String(localized: "Received your message · \(time)")
            case .presenceSeen:
                return String(localized: "Seen online · \(time)")
            case .connected:
                return String(localized: "Last connected · \(time)")
            case .disconnected:
                return String(localized: "Last disconnected · \(time)")
            }
        }
        switch latest.evidence {
        // "Sent you a message" is THEIR message landing here; "Received your
        // message" is a message THIS phone sent arriving at theirs. Swapping
        // them is the bug this wording exists to prevent.
        case .messageReceived:
            return String(localized: "Sent you a message via \(path) · \(time)")
        case .messageDelivered:
            return String(localized: "Received your message via \(path) · \(time)")
        case .presenceSeen:
            return String(localized: "Seen online through \(path) · \(time)")
        case .connected:
            return String(localized: "Last connected via \(path) · \(time)")
        case .disconnected:
            return String(localized: "Last disconnected from \(path) · \(time)")
        }
    }

    private func eventText(_ event: PeerConnectionEvent) -> String {
        let name = contacts.first(where: { $0.userId == event.userId })
            .map { coreContactDisplayName(contact: $0) } ?? String(localized: "Friend")
        let time = formatTime(event.occurredAtMs)
        guard let path = transportLabel(event.transport) else {
            switch ConnectionActivityLogic.evidence(of: event.kind) {
            case .messageReceived:
                return String(localized: "\(name) sent you a message · \(time)")
            case .messageDelivered:
                return String(localized: "\(name) received your message · \(time)")
            case .presenceSeen:
                return String(localized: "\(name) was reachable · \(time)")
            case .connected:
                return String(localized: "\(name) connected · \(time)")
            case .disconnected:
                return String(localized: "\(name) disconnected · \(time)")
            }
        }
        switch ConnectionActivityLogic.evidence(of: event.kind) {
        case .messageReceived:
            return String(localized: "\(name) sent you a message via \(path) · \(time)")
        case .messageDelivered:
            return String(localized: "\(name) received your message via \(path) · \(time)")
        case .presenceSeen:
            return String(localized: "\(name) was reachable via \(path) · \(time)")
        case .connected:
            return String(localized: "\(name) connected via \(path) · \(time)")
        case .disconnected:
            return String(localized: "\(name) disconnected via \(path) · \(time)")
        }
    }

    /// The copy naming this path, or nil when there is no path to name.
    ///
    /// Nil exactly when core says the path was not observed
    /// (`corePeerTransportIsObserved`) -- pinned by
    /// `ConnectionActivityLogicTests`, so the two cannot drift apart. A caller
    /// that gets nil must switch to the wordless variant rather than
    /// substituting a plausible-looking radio; that substitution is the bug
    /// this screen was fixed for. Mirrors `transportLabelId` in
    /// ConnectionDetailsScreen.kt.
    static func transportLabel(_ transport: PeerConnectionTransport) -> String? {
        switch transport {
        case .bluetooth: return String(localized: "Bluetooth")
        case .localWifi: return String(localized: "local Wi-Fi")
        case .shorePass: return String(localized: "Shore Pass")
        case .carried: return nil
        }
    }

    private func transportLabel(_ transport: PeerConnectionTransport) -> String? {
        Self.transportLabel(transport)
    }

    private func formatTime(_ milliseconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(milliseconds) / 1_000)
            .formatted(date: .numeric, time: .shortened)
    }
}
