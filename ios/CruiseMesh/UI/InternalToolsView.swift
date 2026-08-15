import SwiftUI
import UIKit

/// Reachable on a debug build outright, and on a release build once someone has
/// done the seven-tap run on the version line in Settings. It has to be
/// reachable on release: a TestFlight build is signed for release, and a
/// staged-rollout canary whose switches only exist in a developer's own build
/// can never produce the field evidence it exists to produce.
struct InternalToolsView: View {
    @ObservedObject var appModel: AppModel
    /// Told when this screen hides itself again, so the Settings row it was
    /// reached from can disappear with it.
    var onLock: () -> Void = {}
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var lanDiagnostics = LanTransportDiagnostics.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared

    @State private var relayUrl = ""
    @State private var relayToken = ""
    @State private var useCoreRelayEngine = false
    @State private var relayShadowOn = true
    @State private var useCoreInboundEngine = false
    @State private var lanAddress = ""
    @State private var lanError: String?
    @State private var showLanQR = false
    @State private var showLanScanner = false
    @State private var shareFile: ShareableFile?

    var body: some View {
        // Sections are separate `some View` builders so the type checker
        // never has to solve the entire Form as one expression (Xcode 26
        // times out on that).
        Form {
            releaseWarningSection
            relaySection
            receiveSection
            lanFieldToolsSection
            diagnosticsSection
            lockAgainSection
        }
        .navigationTitle("Internal tools")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            if let config = RelayConfigStore.load() {
                relayUrl = config.relayUrl
                relayToken = config.relayToken
            }
            useCoreRelayEngine = RelayEngineSettings.passEngine() == .core
            relayShadowOn = RelayEngineSettings.shadowEnabled()
            useCoreInboundEngine = InboundEngineSettings.pathEngine() == .core
        }
        .task(id: relayUrl + "\u{0}" + relayToken) {
            try? await Task.sleep(nanoseconds: 350_000_000)
            guard !Task.isCancelled else { return }
            RelayConfigStore.save(relayUrl: relayUrl, relayToken: relayToken)
        }
        .sheet(isPresented: $showLanQR) {
            if let endpointText = lanDiagnostics.snapshot.localEndpoint,
               let endpoint = parseLanManualEndpoint(endpointText) {
                LanEndpointQRView(endpoint: endpoint)
            }
        }
        .sheet(item: $shareFile) { file in
            ActivityShareView(items: file.urls)
        }
        .sheet(isPresented: $showLanScanner) {
            QRScannerView { code in
                let fragment = URL(string: code)?.fragment ?? code
                guard let endpoint = parseLanEndpointLink(fragment) else {
                    lanError = "That QR code is not a CruiseMesh LAN address"
                    return
                }
                showLanScanner = false
                if !appModel.meshEnabled { appModel.startMesh() }
                LanTransportDiagnostics.shared.queueManualConnection(endpoint)
                lanAddress = endpoint.display
                lanError = nil
            }
        }
    }

    /// On a release build these switches are only here because someone did the
    /// seven-tap run. Say plainly what they do before they are touched. A debug
    /// build skips it -- a developer knows.
    @ViewBuilder
    private var releaseWarningSection: some View {
        if internalToolsUnlockedOnRelease {
            Section {
                Text("These switches change how your messages are delivered. If nobody asked you to change one, leave them as they are. The safe position is off.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// The way back out, for anyone who would rather not hunt for the version
    /// line again. Only offered where it can do anything: a debug build shows
    /// this screen regardless of the flag.
    @ViewBuilder
    private var lockAgainSection: some View {
        if internalToolsUnlockedOnRelease {
            Section {
                Button("Hide internal tools", role: .destructive) {
                    InternalToolsUnlockStore.setUnlocked(false)
                    RelayConfigStore.save(relayUrl: relayUrl, relayToken: relayToken)
                    onLock()
                    dismiss()
                }
            }
        }
    }

    @ViewBuilder
    private var relaySection: some View {
        Section("Relay") {
            TextField("Relay URL", text: $relayUrl)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            // The core drops a non-HTTPS URL on save rather than storing
            // it. Say so here: this field writes on every keystroke, so
            // without a message the value just never takes effect.
            if relayUrlIsInsecure(value: relayUrl) {
                Text("Relay URL must start with https:// This one was not saved.")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            SecureField("Family token", text: $relayToken)
            Text("When any family phone has internet, queued messages flush through this mailbox.")
                .font(.caption)
                .foregroundStyle(.secondary)
            if case .tokenRejected = connectivity.relay {
                Text(tokenRejectionMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            // C2: the whole-pass rollback switch, read once when a pass starts.
            // Off by default; the legacy engine is unchanged until this is
            // turned on. Mirrors Android's "Rebuilt internet sync" toggle.
            Toggle("Rebuilt internet sync", isOn: Binding(
                get: { useCoreRelayEngine },
                set: {
                    useCoreRelayEngine = $0
                    RelayEngineSettings.setPassEngine($0 ? .core : .legacy)
                }
            ))
            Text("Runs the next relay pass on the rebuilt core engine. Legacy stays the default; this is for canary testing only.")
                .font(.caption)
                .foregroundStyle(.secondary)
            // Same words as Android's switch, in plain language: a tester on a
            // release build reads this, not a developer.
            Toggle("Relay migration check", isOn: Binding(
                get: { relayShadowOn },
                set: {
                    relayShadowOn = $0
                    RelayEngineSettings.setShadowEnabled($0)
                }
            ))
            Text("On a few internet syncs a day, compares what the rebuilt code would have done and records only where they differ. Nothing extra is sent or received.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var receiveSection: some View {
        Section("Receiving") {
            // The per-envelope rollback switch, read once as each frame
            // arrives. Off by default; the legacy receive path is unchanged
            // until this is turned on.
            Toggle("Rebuilt message handling", isOn: Binding(
                get: { useCoreInboundEngine },
                set: {
                    useCoreInboundEngine = $0
                    InboundEngineSettings.setPathEngine($0 ? .core : .legacy)
                }
            ))
            Text("Handles arriving messages with the rebuilt shared engine. The old path stays the default; this is for testing only.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// Shore Pass vs generic relay copy for a rejected family token.
    /// Uses the core's `relaySetupIsOfficial` (same check as ShorePassView /
    /// MeshStatusPillLogic) against the URL currently in the field.
    private var tokenRejectionMessage: String {
        if relaySetupIsOfficial(relayUrl: relayUrl) {
            return String(localized: "Shore Pass rejected this family token. Messages will wait until the token is fixed — check it against another family phone.")
        }
        return String(localized: "The relay rejected this family token. Messages will wait until the token is fixed — check it against another family phone.")
    }

    @ViewBuilder
    private var lanFieldToolsSection: some View {
        Section("Local Wi-Fi field tools") {
            Text("Keep Wi-Fi connected even when it has no internet — CruiseMesh uses it to reach phones near you.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(lanDiagnostics.snapshot.state)
            lanLocalEndpointRows
            if !lanDiagnostics.snapshot.activePeerNames.isEmpty {
                Text("Secure link: \(lanDiagnostics.snapshot.activePeerNames.joined(separator: ", "))")
                    .foregroundStyle(.tint)
            }
            if let endpoint = lanDiagnostics.snapshot.lastPeerEndpoint {
                LabeledContent("Last peer", value: endpoint).font(.caption)
            }
            TextField("Friend IP address", text: $lanAddress)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.numbersAndPunctuation)
            Text("The port is optional. An accepted friend and encrypted identity check are still required.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button("Connect securely") {
                if !appModel.meshEnabled { appModel.startMesh() }
                lanError = lanDiagnostics.requestManualConnection(lanAddress)
            }
            Button("Test encrypted LAN link") { lanError = lanDiagnostics.requestConnectionTest() }
            Button("Search local subnet") { lanError = lanDiagnostics.requestSubnetScan() }
            lanScanAndSweepRows
            Text("Encrypted frames: \(lanDiagnostics.snapshot.sentFrames) sent · \(lanDiagnostics.snapshot.receivedFrames) received")
                .font(.caption)
                .foregroundStyle(.secondary)
            if let error = lanError ?? lanDiagnostics.snapshot.lastError {
                Text(error).font(.caption).foregroundStyle(.red)
            }
        }
    }

    @ViewBuilder
    private var lanLocalEndpointRows: some View {
        if let endpoint = lanDiagnostics.snapshot.localEndpoint {
            LabeledContent("This phone", value: endpoint).font(.footnote.monospaced())
            Button("Copy this phone's address") { UIPasteboard.general.string = endpoint }
            HStack {
                Button("Show address QR") { showLanQR = true }
                Spacer()
                Button("Scan address QR") { showLanScanner = true }
            }
        }
    }

    @ViewBuilder
    private var lanScanAndSweepRows: some View {
        if let total = lanDiagnostics.snapshot.scanTotal {
            ProgressView(
                value: Double(lanDiagnostics.snapshot.scanProgress ?? 0),
                total: Double(total)
            ) {
                Text("Checked \(lanDiagnostics.snapshot.scanProgress ?? 0) of \(total) addresses")
                    .font(.caption)
            }
        }
        if let probe = lanDiagnostics.snapshot.probeStatus {
            Text(probe).font(.caption).foregroundStyle(.secondary)
        }
        switch lanDiagnostics.snapshot.sweepDisplayState {
        case .none:
            EmptyView()
        case .checking:
            Text("Checking this network…")
                .font(.caption)
                .foregroundStyle(.secondary)
        case .isolationSuspected:
            Text("This Wi-Fi appears to block phone-to-phone traffic; nearby delivery will use Bluetooth.")
                .font(.caption)
                .foregroundStyle(.red)
        case .blockedByPolicy:
            Text("Local Wi-Fi probes were denied, likely by a VPN or OS policy; nearby delivery will use Bluetooth.")
                .font(.caption)
                .foregroundStyle(.red)
        }
    }

    @ViewBuilder
    private var diagnosticsSection: some View {
        Section("Diagnostics") {
            Button {
                // Bundles any MetricKit payloads in alongside the log
                // file, riding this existing flow with zero new UI.
                var urls: [URL] = []
                if let url = DiagnosticLogExport.writeLogFile() { urls.append(url) }
                urls.append(contentsOf: DiagnosticLogExport.metricKitFileURLs())
                if urls.isEmpty {
                    lanError = "No diagnostics captured this session yet"
                } else {
                    shareFile = ShareableFile(urls: urls)
                }
            } label: {
                Label("Share diagnostics", systemImage: "ladybug")
            }
            Text("Shares this session's connection and delivery log to help debug mesh problems. Metadata only — no message content.")
                .font(.caption)
                .foregroundStyle(.secondary)

            Button {
                if let url = FieldMetricsExport.writeCSVFile() {
                    shareFile = ShareableFile(url: url)
                } else {
                    lanError = "No field metrics captured yet"
                }
            } label: {
                Label("Export field metrics", systemImage: "square.and.arrow.up")
            }
            Text("Exports a CSV of delivery timings and the transports messages used, for cruise-test analysis. Metadata only — no message content or contact names.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

private struct LanEndpointQRView: View {
    let endpoint: LanManualEndpoint
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            VStack(spacing: 18) {
                let link = lanEndpointLink(endpoint)
                if let image = QRCodeGenerator.image(from: link, size: 260) {
                    Image(uiImage: image)
                        .interpolation(.none)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 260, height: 260)
                        .padding()
                        .background(RoundedRectangle(cornerRadius: 16).fill(Color.white))
                }
                Text(endpoint.display).font(.body.monospaced())
                Text("Your friend must already be accepted. The QR only supplies a local network address; the encrypted identity check still applies.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
                ShareLink(item: link) {
                    Label("Share address", systemImage: "square.and.arrow.up")
                }
                Spacer()
            }
            .padding()
            .navigationTitle("Local Wi-Fi address")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}
