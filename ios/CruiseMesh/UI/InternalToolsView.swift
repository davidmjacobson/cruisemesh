import SwiftUI
import UIKit

#if DEBUG
struct InternalToolsView: View {
    @ObservedObject var appModel: AppModel
    @ObservedObject private var lanDiagnostics = LanTransportDiagnostics.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared

    @State private var relayUrl = ""
    @State private var relayToken = ""
    @State private var lanAddress = ""
    @State private var lanError: String?
    @State private var showLanQR = false
    @State private var showLanScanner = false
    @State private var shareFile: ShareableFile?

    var body: some View {
        Form {
            Section("Relay") {
                TextField("Relay URL", text: $relayUrl)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                SecureField("Family token", text: $relayToken)
                Text("When any family phone has internet, queued messages flush through this mailbox.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if case .tokenRejected = connectivity.relay {
                    Text("The relay rejected this family token. Messages will wait until the token is fixed — check it against another family phone.")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }

            Section("Local Wi-Fi field tools") {
                Text("Keep Wi-Fi connected even when it has no internet — CruiseMesh uses it to reach phones near you.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(lanDiagnostics.snapshot.state)
                if let endpoint = lanDiagnostics.snapshot.localEndpoint {
                    LabeledContent("This phone", value: endpoint).font(.footnote.monospaced())
                    Button("Copy this phone's address") { UIPasteboard.general.string = endpoint }
                    HStack {
                        Button("Show address QR") { showLanQR = true }
                        Spacer()
                        Button("Scan address QR") { showLanScanner = true }
                    }
                }
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
                Text("Encrypted frames: \(lanDiagnostics.snapshot.sentFrames) sent · \(lanDiagnostics.snapshot.receivedFrames) received")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let error = lanError ?? lanDiagnostics.snapshot.lastError {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }

            Section("Diagnostics") {
                Button {
                    if let url = DiagnosticLogExport.writeLogFile() {
                        shareFile = ShareableFile(url: url)
                    } else {
                        lanError = "No diagnostics captured this session yet"
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
        .navigationTitle("Internal tools")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            if let config = RelayConfigStore.load() {
                relayUrl = config.relayUrl
                relayToken = config.relayToken
            }
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
            ActivityShareView(items: [file.url])
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
#endif
