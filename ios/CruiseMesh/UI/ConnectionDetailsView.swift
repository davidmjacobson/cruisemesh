import SwiftUI

struct ConnectionDetailsView: View {
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var contacts: [Contact] = []
    @State private var summaries: [PeerConnectionSummary] = []
    @State private var events: [PeerConnectionEvent] = []
    @State private var showClear = false
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
                        ForEach(Array(events.enumerated()), id: \.offset) { _, event in
                            Text(eventText(event)).font(.caption)
                        }
                    }
                }

                Section("Support") {
                    Button {
                        if let url = DiagnosticLogExport.writeLogFile() {
                            shareFile = ShareableFile(url: url)
                        } else {
                            supportMessage = "No diagnostics captured this session yet."
                        }
                    } label: {
                        Label("Share diagnostics", systemImage: "ladybug")
                    }
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
            return "Connected now via local Wi-Fi"
        case .bluetooth:
            return "Connected now via Bluetooth"
        case nil:
            break
        }
        let rows = summaries.filter { $0.userId == contact.userId }
        guard let latest = rows.max(by: { summaryTime($0) < summaryTime($1) }),
              summaryTime(latest) > 0 else {
            return "No connection history yet"
        }
        return summaryText(latest)
    }

    private func eventText(_ event: PeerConnectionEvent) -> String {
        let name = contacts.first(where: { $0.userId == event.userId })
            .map { coreContactDisplayName(contact: $0) } ?? "Friend"
        let action: String
        switch event.kind {
        case .connected: action = "connected"
        case .disconnected: action = "disconnected"
        case .presenceSeen: action = "was reachable"
        case .messageDelivered: action = "message arrived"
        }
        return "\(name) \(action) via \(transportLabel(event.transport)) · \(formatTime(event.occurredAtMs))"
    }

    private func summaryTime(_ summary: PeerConnectionSummary) -> Int64 {
        [
            summary.lastConnectedAtMs,
            summary.lastDisconnectedAtMs,
            summary.lastSeenAtMs,
            summary.lastDeliveredAtMs,
        ].compactMap { $0 }.max() ?? 0
    }

    private func summaryText(_ summary: PeerConnectionSummary) -> String {
        let timestamp = summaryTime(summary)
        let evidence: String
        if summary.lastDeliveredAtMs == timestamp {
            evidence = "Message arrived via \(transportLabel(summary.transport))"
        } else if summary.lastSeenAtMs == timestamp {
            evidence = "Seen online through \(transportLabel(summary.transport))"
        } else if summary.lastConnectedAtMs == timestamp {
            evidence = "Last connected via \(transportLabel(summary.transport))"
        } else {
            evidence = "Last disconnected from \(transportLabel(summary.transport))"
        }
        return "\(evidence) · \(formatTime(timestamp))"
    }

    private func transportLabel(_ transport: PeerConnectionTransport) -> String {
        switch transport {
        case .bluetooth: return "Bluetooth"
        case .localWifi: return "local Wi-Fi"
        case .cruisePass: return "Cruise Pass"
        }
    }

    private func formatTime(_ milliseconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(milliseconds) / 1_000)
            .formatted(date: .numeric, time: .shortened)
    }
}
