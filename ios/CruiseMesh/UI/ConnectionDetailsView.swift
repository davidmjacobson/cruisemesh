import SwiftUI

struct ConnectionDetailsView: View {
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var contacts: [Contact] = []
    @State private var summaries: [PeerConnectionSummary] = []
    @State private var events: [PeerConnectionEvent] = []
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
                    LabeledContent("Cruise Pass", value: relayLabel)
                    Text("CruiseMesh chooses the best available path automatically. A message may arrive by Bluetooth, local Wi-Fi, or Cruise Pass.")
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
                    Text("Turn this on before testing to keep connection and delivery diagnostics across app restarts. Message content is never recorded.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button {
                        if let url = DiagnosticLogExport.writeLogFile() {
                            shareFile = ShareableFile(url: url)
                            hasDiagnosticArchive = true
                        } else {
                            supportMessage = String(localized: "No diagnostics captured this session yet.")
                        }
                    } label: {
                        Label("Share diagnostics", systemImage: "ladybug")
                    }
                    Button(role: .destructive) {
                        DiagnosticLogExport.deleteArchive()
                        hasDiagnosticArchive = false
                        supportMessage = String(localized: "Captured diagnostics deleted.")
                    } label: {
                        Label("Delete captured diagnostics", systemImage: "trash")
                    }
                    .disabled(!hasDiagnosticArchive)
                    Button {
                        if let url = FieldMetricsExport.writeCSVFile() {
                            shareFile = ShareableFile(url: url)
                        } else {
                            supportMessage = "No field metrics captured yet."
                        }
                    } label: {
                        Label("Export field metrics", systemImage: "square.and.arrow.up")
                    }
                    Button("Clear connection history", role: .destructive) {
                        showClear = true
                    }
                    Text("History contains only friend identity, path type, event type, and time. It never stores message content, relay tokens, IP addresses, or Wi-Fi names.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("Field metrics contain hashed chat tags, route types, and delivery timings—never message content or contact names.")
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
                ActivityShareView(items: [file.url])
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
        guard let latest = ConnectionActivityLogic.latestPeerStatus(rows) else {
            return String(localized: "No connection history yet")
        }
        let path = transportLabel(latest.transport)
        let time = formatTime(latest.atMs)
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
        let path = transportLabel(event.transport)
        let time = formatTime(event.occurredAtMs)
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

    private func transportLabel(_ transport: PeerConnectionTransport) -> String {
        switch transport {
        case .bluetooth: return String(localized: "Bluetooth")
        case .localWifi: return String(localized: "local Wi-Fi")
        case .cruisePass: return String(localized: "Cruise Pass")
        }
    }

    private func formatTime(_ milliseconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(milliseconds) / 1_000)
            .formatted(date: .numeric, time: .shortened)
    }
}
