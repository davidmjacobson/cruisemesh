import PhotosUI
import SwiftUI

struct ProfileView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    @State private var displayName = ""
    @State private var meshOn = true
    @State private var shareOnline = true
    @State private var avatarImage: UIImage?
    @State private var photoItem: PhotosPickerItem?
    @State private var friendsOfFriends = true
    @State private var showMyCard = false

    var body: some View {
        NavigationStack {
            Form {
                Section("You") {
                    HStack {
                        Spacer()
                        AvatarView(
                            userId: identity.userId,
                            name: displayName,
                            size: 80,
                            photo: avatarImage
                        )
                        Spacer()
                    }
                    PhotosPicker(selection: $photoItem, matching: .images) {
                        Label("Choose profile photo", systemImage: "photo")
                    }
                    if avatarImage != nil {
                        Button("Remove profile photo", role: .destructive) {
                            ProfilePhotoStore.clear()
                            avatarImage = nil
                            syncProfile(epoch: ProfileStore.bumpOwnAvatarEpoch())
                        }
                    }
                    TextField("Display name", text: $displayName)
                    DisclosureGroup("Verify my identity") {
                        Text(fingerprintWords(userId: identity.userId).joined(separator: " "))
                            .font(.body.monospaced())
                            .textSelection(.enabled)
                        Text("Have your friend match these words against your name in their contacts to confirm it's really you.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Section("My friend card") {
                    Button {
                        showMyCard = true
                    } label: {
                        Label("Show my friend card", systemImage: "qrcode")
                    }
                }

                Section("Backup") {
                    NavigationLink {
                        BackupExportView()
                    } label: {
                        Label("Back up account", systemImage: "externaldrive")
                    }
                    Text("Save your identity and messages to an encrypted file.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("Mesh") {
                    Toggle("Mesh running", isOn: $meshOn)
                        .onChange(of: meshOn) { on in
                            if on { appModel.startMesh() } else { appModel.stopMesh() }
                        }
                    LabeledContent("Status", value: runtime.pillText)
                    Toggle("Share when I'm online", isOn: $shareOnline)
                        .onChange(of: shareOnline) { RelayConfigStore.setShareOnline($0) }
                }

                Section("Privacy") {
                    Toggle("Friends of friends", isOn: $friendsOfFriends)
                        .onChange(of: friendsOfFriends) { updateFriendsOfFriends($0) }
                    Text("Let friends introduce you to people they know. Messages and phone contacts are never shared.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("Advanced") {
                    NavigationLink {
                        AdvancedSettingsView(appModel: appModel)
                    } label: {
                        Label("Relay, local Wi-Fi, and diagnostics", systemImage: "gearshape.2")
                    }
                }

                Section("Legal") {
                    Button("Terms of Use") { openURL(TermsAcceptanceStore.termsURL) }
                    Button("Privacy policy") { openURL(TermsAcceptanceStore.privacyURL) }
                }
            }
            .navigationTitle("Profile & settings")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear {
                displayName = appModel.displayName
                meshOn = appModel.meshEnabled
                shareOnline = RelayConfigStore.shareOnline()
                avatarImage = ProfilePhotoStore.loadAvatarImage()
                friendsOfFriends = FriendsOfFriendsStore.isEnabled()
            }
            .task(id: displayName) {
                try? await Task.sleep(nanoseconds: 350_000_000)
                guard !Task.isCancelled else { return }
                let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
                guard trimmed != appModel.displayName else { return }
                ProfileStore.saveDisplayName(trimmed)
                appModel.displayName = trimmed
                syncProfile(epoch: ProfileStore.bumpOwnAvatarEpoch())
            }
            .onChange(of: photoItem) { item in
                guard let item else { return }
                Task {
                    guard let data = try? await item.loadTransferable(type: Data.self),
                          let image = UIImage(data: data),
                          let saved = ProfilePhotoStore.save(image: image) else { return }
                    await MainActor.run {
                        avatarImage = saved
                        syncProfile(epoch: ProfileStore.bumpOwnAvatarEpoch())
                    }
                }
            }
            .sheet(isPresented: $showMyCard) {
                MyQRView(identity: identity, displayName: displayName, onSayHi: { _ in })
            }
        }
    }

    private func syncProfile(epoch: Int64) {
        ProfileSyncSender.queueToAllContacts(
            store: AppStore.get(),
            identity: identity,
            displayName: displayName.trimmingCharacters(in: .whitespacesAndNewlines),
            epoch: epoch
        )
    }

    private func updateFriendsOfFriends(_ enabled: Bool) {
        guard FriendsOfFriendsStore.isEnabled() != enabled else { return }
        FriendsOfFriendsStore.setEnabled(enabled)
        if !enabled { try? AppStore.get().clearFriendSuggestions() }
        syncProfile(epoch: ProfileStore.loadOwnAvatarEpoch())
        FriendDirectorySender.queueToAllContacts(store: AppStore.get(), identity: identity)
    }
}

private struct AdvancedSettingsView: View {
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

            Section("Local Wi-Fi (experimental)") {
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
        .navigationTitle("Advanced settings")
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
