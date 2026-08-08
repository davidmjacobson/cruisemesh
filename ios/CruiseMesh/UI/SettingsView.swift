import SwiftUI

struct SettingsView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var shareOnline = RelayConfigStore.shareOnline()
    @State private var friendsOfFriends = FriendsOfFriendsStore.isEnabled()

    var body: some View {
        NavigationStack {
            Form {
                Section("Shore Pass") {
                    NavigationLink {
                        ShorePassView(initialCard: nil, appModel: appModel)
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(relayTitle)
                                Text(relayDetail)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            if let symbol = passIndicator.systemImage {
                                Spacer()
                                Image(systemName: symbol)
                                    .foregroundStyle(passIndicator.tint)
                                    .accessibilityLabel(
                                        passIndicator.accessibilityLabel ?? ""
                                    )
                            }
                        }
                    }
                }

                Section("CruiseMesh operation") {
                    Toggle(
                        "Mesh running",
                        isOn: Binding(
                            get: { appModel.meshEnabled },
                            set: { $0 ? appModel.startMesh() : appModel.stopMesh() }
                        )
                    )
                    LabeledContent("Status", value: runtime.pillText)
                    NavigationLink {
                        ConnectionDetailsView(appModel: appModel)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Connection details")
                            Text("Active paths, people, recent activity, and support diagnostics.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
#if DEBUG
                    NavigationLink {
                        InternalToolsView(appModel: appModel)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Internal field tools")
                            Text("Manual local-network probes, raw route counters, and diagnostic exports.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
#endif
                }

                Section("Privacy") {
                    Toggle("Friends of friends", isOn: $friendsOfFriends)
                        .onChange(of: friendsOfFriends) { updateFriendsOfFriends($0) }
                    Text("Let friends introduce you to people they know, and share your contact card with someone they choose. Messages and phone contacts are never shared.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Toggle("Share when I'm online", isOn: $shareOnline)
                        .onChange(of: shareOnline) { RelayConfigStore.setShareOnline($0) }
                }

                Section("Backup") {
                    NavigationLink {
                        BackupExportView()
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Label("Back up account", systemImage: "externaldrive")
                            Text("Export an encrypted copy of your identity and messages.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Section("About & legal") {
                    Link("Help & support", destination: URL(string: "https://cruisemesh.app/support/")!)
                    Link("Terms of Use", destination: TermsAcceptanceStore.termsURL)
                    Link("Privacy policy", destination: TermsAcceptanceStore.privacyURL)
                }

                Section {
                    VStack(spacing: 6) {
                        // verbatim: a version string is data, not copy, and
                        // must not land in the localization catalog.
                        Text(verbatim: versionLabel)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        // The author's dedication, in the traditional place for
                        // one: the very bottom of the last screen, after
                        // everything functional. Latin and untranslated -- a
                        // fixed phrase, the way Bach's manuscripts carry it.
                        Text(verbatim: "Soli Deo gloria")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                    .frame(maxWidth: .infinity)
                    .listRowBackground(Color.clear)
                }
            }
            .navigationTitle("Settings")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .accessibilityIdentifier("screen.settings")
        }
    }

    private var passIndicator: PassIndicator {
        PassIndicator.of(connectivity.relay, configured: RelayConfigStore.load() != nil)
    }

    /// "CruiseMesh 1.0.2 (1784978966)". The build number is the part that
    /// identifies a build in a bug report: `CFBundleShortVersionString` falls
    /// back to a hardcoded value for anything not built from a release tag, so
    /// it is not unique on its own.
    private var versionLabel: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "CruiseMesh \(short) (\(build))"
    }

    private var relayTitle: String {
        guard RelayConfigStore.load() != nil else { return "Set up Shore Pass" }
        switch connectivity.relay {
        case .noConfig: return "Checking Shore Pass setup…"
        case .checking: return "Checking Shore Pass setup…"
        case .ok: return "Shore Pass is working"
        case .noInternet: return "Shore Pass is waiting for internet"
        case .failing: return "Shore Pass needs attention"
        case .expired: return "Shore Pass expired"
        case .suspended: return "Shore Pass suspended"
        case .tokenRejected: return "Shore Pass setup was rejected"
        case .quotaFull: return String(localized: "Shore Pass storage is full")
        case .messageTooLarge: return String(localized: "A message is too large to send")
        case .rateLimited: return String(localized: "Shore Pass is catching up")
        }
    }

    private var relayDetail: String {
        guard RelayConfigStore.load() != nil else {
            return "CruiseMesh still works nearby. Add a pass for internet delivery."
        }
        switch connectivity.relay {
        case .noConfig: return "Setup is saved and will be checked when CruiseMesh runs."
        case .checking: return "Setup is saved; CruiseMesh has not completed an authenticated check yet."
        case .ok(let lastSyncMs): return "Internet delivery is ready · checked \(relativeAge(lastSyncMs))."
        case .noInternet: return "Configured; this phone is currently offline."
        case .failing: return "The relay could not be reached."
        case .expired: return "Renew your pass to resume internet delivery."
        case .suspended: return "Contact support for help with this pass."
        case .tokenRejected: return "Paste the setup card again, or use a different Shore Pass."
        case .quotaFull:
            return String(localized: "Internet delivery is paused until your family collects waiting messages.")
        case .messageTooLarge:
            return String(localized: "One message can’t be sent over internet delivery. Other messages still deliver.")
        case .rateLimited:
            return String(localized: "Syncing is slowed right now. It recovers on its own.")
        }
    }

    private func relativeAge(_ timestampMs: Int64) -> String {
        let minutes = max(0, (Int64(Date().timeIntervalSince1970 * 1_000) - timestampMs) / 60_000)
        if minutes == 0 { return "just now" }
        if minutes < 60 { return "\(minutes)m ago" }
        if minutes < 24 * 60 { return "\(minutes / 60)h ago" }
        return "\(minutes / (24 * 60))d ago"
    }

    private func updateFriendsOfFriends(_ enabled: Bool) {
        guard FriendsOfFriendsStore.isEnabled() != enabled else { return }
        FriendsOfFriendsStore.setEnabled(enabled)
        let store = AppStore.get()
        if !enabled { try? store.clearFriendSuggestions() }
        ProfileSyncSender.queueToAllContacts(
            store: store,
            identity: identity,
            displayName: appModel.displayName,
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
    }
}
