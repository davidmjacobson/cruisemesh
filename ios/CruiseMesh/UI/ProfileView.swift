import PhotosUI
import SwiftUI

/// Hosted privacy policy (App Store / Play Console + in-app link).
private let privacyPolicyURL = URL(string: "https://cruisemesh.app/privacy")!

struct ProfileView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    @State private var displayName = ""
    @State private var lastSyncedName = ""
    @State private var meshOn = true
    @State private var avatarImage: UIImage?
    @State private var photoItem: PhotosPickerItem?
    @State private var friendsOfFriends = true
    @State private var shareOnline = true
    @State private var showMyQR = false
    @State private var hasLoaded = false
    @State private var profileSyncTask: Task<Void, Never>?

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
                            let epoch = ProfileStore.bumpOwnAvatarEpoch()
                            ProfileSyncSender.queueToAllContacts(
                                store: AppStore.get(),
                                identity: identity,
                                displayName: displayName,
                                epoch: epoch
                            )
                        }
                    }
                    TextField("Display name", text: $displayName)
                }

                Section("My friend card") {
                    LabeledContent("User ID", value: formatUserId(userId: identity.userId))
                    LabeledContent(
                        "Fingerprint",
                        value: fingerprintWords(userId: identity.userId).joined(separator: " ")
                    )
                    Text("Read these words aloud with your friend to verify keys.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button {
                        showMyQR = true
                    } label: {
                        Label("Show my friend card", systemImage: "qrcode")
                    }
                }

                Section("Backup") {
                    LabeledContent("Local backup", value: "Coming soon")
                    Text("Encrypted backup and cross-platform restore will appear here in the next backup update.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("Mesh") {
                    Toggle("Mesh running", isOn: $meshOn)
                    LabeledContent("Status", value: runtime.pillText)
                    Toggle("Share when I'm online", isOn: $shareOnline)
                    Text("Help family messages move through the relay when this phone has internet.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("Privacy") {
                    Toggle("Friends of friends", isOn: $friendsOfFriends)
                    Text("Let friends introduce you to people they know. Your messages and phone contacts are never shared.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Section("Advanced") {
                    NavigationLink {
                        AdvancedSettingsView(appModel: appModel)
                    } label: {
                        Label("Relay, local Wi-Fi, and diagnostics", systemImage: "slider.horizontal.3")
                    }
                }

                Section("Legal") {
                    Button("Privacy policy") {
                        openURL(privacyPolicyURL)
                    }
                }
            }
            .navigationTitle("Profile & settings")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") {
                        flushProfileSync()
                        dismiss()
                    }
                }
            }
            .onAppear {
                displayName = appModel.displayName
                lastSyncedName = appModel.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
                meshOn = appModel.meshEnabled
                avatarImage = ProfilePhotoStore.loadAvatarImage()
                friendsOfFriends = FriendsOfFriendsStore.isEnabled()
                shareOnline = RelayConfigStore.shareOnline()
                hasLoaded = true
            }
            .onDisappear {
                flushProfileSync()
            }
            .onChange(of: displayName) { value in
                guard hasLoaded else { return }
                ProfileStore.saveDisplayName(value)
                appModel.displayName = value
                scheduleProfileSync()
            }
            .onChange(of: meshOn) { on in
                guard hasLoaded else { return }
                if on { appModel.startMesh() } else { appModel.stopMesh() }
            }
            .onChange(of: shareOnline) { enabled in
                guard hasLoaded else { return }
                RelayConfigStore.setShareOnline(enabled)
            }
            .onChange(of: friendsOfFriends) { enabled in
                guard hasLoaded else { return }
                updateFriendsOfFriends(enabled)
            }
            .onChange(of: photoItem) { item in
                guard let item else { return }
                Task {
                    guard let data = try? await item.loadTransferable(type: Data.self),
                          let image = UIImage(data: data),
                          let saved = ProfilePhotoStore.save(image: image) else { return }
                    await MainActor.run {
                        avatarImage = saved
                        let epoch = ProfileStore.bumpOwnAvatarEpoch()
                        ProfileSyncSender.queueToAllContacts(
                            store: AppStore.get(),
                            identity: identity,
                            displayName: displayName,
                            epoch: epoch
                        )
                    }
                }
            }
            .sheet(isPresented: $showMyQR) {
                MyQRView(identity: identity, displayName: displayName) { _ in
                    showMyQR = false
                }
            }
        }
    }

    private func scheduleProfileSync() {
        profileSyncTask?.cancel()
        profileSyncTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 400_000_000)
            guard !Task.isCancelled else { return }
            syncProfileIfNeeded()
        }
    }

    private func flushProfileSync() {
        profileSyncTask?.cancel()
        profileSyncTask = nil
        syncProfileIfNeeded()
    }

    private func syncProfileIfNeeded() {
        guard hasLoaded else { return }
        let trimmedName = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmedName != lastSyncedName else { return }
        ProfileStore.saveDisplayName(trimmedName)
        appModel.displayName = trimmedName
        let epoch = ProfileStore.bumpOwnAvatarEpoch()
        ProfileSyncSender.queueToAllContacts(
            store: AppStore.get(),
            identity: identity,
            displayName: trimmedName,
            epoch: epoch
        )
        lastSyncedName = trimmedName
    }

    private func updateFriendsOfFriends(_ enabled: Bool) {
        let changed = FriendsOfFriendsStore.isEnabled() != enabled
        guard changed else { return }
        FriendsOfFriendsStore.setEnabled(enabled)
        if !enabled { try? AppStore.get().clearFriendSuggestions() }
        ProfileSyncSender.queueToAllContacts(
            store: AppStore.get(),
            identity: identity,
            displayName: displayName.trimmingCharacters(in: .whitespacesAndNewlines),
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        FriendDirectorySender.queueToAllContacts(
            store: AppStore.get(),
            identity: identity
        )
    }
}

private struct AdvancedSettingsView: View {
    @ObservedObject var appModel: AppModel
    @ObservedObject private var lanDiagnostics = LanTransportDiagnostics.shared

    @State private var relayUrl = ""
    @State private var relayToken = ""
    @State private var lanAddress = ""
    @State private var lanError: String?
    @State private var showLanQR = false
    @State private var showLanScanner = false
    @State private var hasLoaded = false

    var body: some View {
        Form {
            Section("Relay") {
                TextField("Relay URL", text: $relayUrl)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                SecureField("Family token", text: $relayToken)
                Text("Optional family mailbox used when phones cannot reach each other directly.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Local Wi-Fi tools") {
                Text(lanDiagnostics.snapshot.state)
                if let endpoint = lanDiagnostics.snapshot.localEndpoint {
                    LabeledContent("This phone", value: endpoint)
                        .font(.footnote.monospaced())
                    Button("Copy this phone's address") {
                        UIPasteboard.general.string = endpoint
                    }
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
                    LabeledContent("Last peer", value: endpoint)
                        .font(.caption)
                }
                TextField("Friend IP address", text: $lanAddress)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.numbersAndPunctuation)
                Text("The port is optional. Manual connection still requires an accepted friend and CruiseMesh's encrypted identity check.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Connect securely") {
                    if !appModel.meshEnabled {
                        appModel.startMesh()
                    }
                    lanError = lanDiagnostics.requestManualConnection(lanAddress)
                }
                Button("Test encrypted LAN link") {
                    lanError = lanDiagnostics.requestConnectionTest()
                }
                Button("Search this /24 network") {
                    lanError = lanDiagnostics.requestSubnetScan()
                }
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
                    Text(probe)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(
                    "Encrypted frames: \(lanDiagnostics.snapshot.sentFrames) sent · \(lanDiagnostics.snapshot.receivedFrames) received"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                if let error = lanError ?? lanDiagnostics.snapshot.lastError {
                    Text(error)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("Advanced")
        .onAppear {
            if let config = RelayConfigStore.load() {
                relayUrl = config.relayUrl
                relayToken = config.relayToken
            }
            hasLoaded = true
        }
        .onChange(of: relayUrl) { _ in saveRelayConfig() }
        .onChange(of: relayToken) { _ in saveRelayConfig() }
        .sheet(isPresented: $showLanQR) {
            if let endpointText = lanDiagnostics.snapshot.localEndpoint,
               let endpoint = parseLanManualEndpoint(endpointText) {
                LanEndpointQRView(endpoint: endpoint)
            }
        }
        .sheet(isPresented: $showLanScanner) {
            QRScannerView { code in
                let fragment = URL(string: code)?.fragment ?? code
                guard let endpoint = parseLanEndpointLink(fragment) else {
                    lanError = "That QR code is not a CruiseMesh LAN address"
                    return
                }
                showLanScanner = false
                if !appModel.meshEnabled {
                    appModel.startMesh()
                }
                LanTransportDiagnostics.shared.queueManualConnection(endpoint)
                lanAddress = endpoint.display
                lanError = nil
            }
        }
    }

    private func saveRelayConfig() {
        guard hasLoaded else { return }
        RelayConfigStore.save(relayUrl: relayUrl, relayToken: relayToken)
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
                Text(endpoint.display)
                    .font(.body.monospaced())
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
