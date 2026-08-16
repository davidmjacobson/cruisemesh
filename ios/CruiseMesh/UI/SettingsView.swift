import SwiftUI
import UIKit

struct SettingsView: View {
    let identity: Identity
    @ObservedObject var appModel: AppModel
    @ObservedObject private var runtime = MeshRuntimeStatus.shared
    @ObservedObject private var connectivity = MeshConnectivityStatus.shared
    @Environment(\.dismiss) private var dismiss

    @State private var shareOnline = RelayConfigStore.shareOnline()
    @State private var friendsOfFriends = FriendsOfFriendsStore.isEnabled()
    /// Debug builds show the developer-settings entry outright, as they always
    /// have. A release build shows it once someone has done the seven-tap run
    /// on the version line at the bottom of this screen.
    ///
    /// Set when the screen opens and deliberately not when a run lands:
    /// inserting a row above the version line pushes the line out from under
    /// the finger still tapping it, and the seventh tap of a run is usually
    /// followed by an eighth on the way to stopping. The entry appears the next
    /// time Settings is opened, which the row's own text says out loud.
    @State private var showDeveloperSettings = developerSettingsVisible
    @State private var unlockTaps = DeveloperSettingsTapCounter()
    /// What the version row reads right now, and the pending revert back to the
    /// version string. One task, cancelled and replaced on every tap, so a run
    /// of taps cannot queue a series of messages that keep arriving after the
    /// tapping has stopped.
    @State private var versionRowLabel: DeveloperSettingsLabel = .version
    @State private var labelRevert: Task<Void, Never>?

    var body: some View {
        NavigationStack {
            Form {
                // First, and permanent: the home-screen card can be dismissed,
                // and this is where somebody who dismissed it goes looking.
                Section {
                    NavigationLink {
                        SailChecklistView(appModel: appModel)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Before you sail")
                            Text("Setup steps that check themselves off, for the days before a cruise.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

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
                    if showDeveloperSettings {
                        NavigationLink {
                            // Popping back does not re-run this view's state,
                            // so the screen tells us when it locked itself
                            // rather than us re-reading the flag on reappear.
                            DeveloperSettingsView(
                                onLock: { showDeveloperSettings = developerSettingsVisible }
                            )
                        } label: {
                            VStack(alignment: .leading, spacing: 3) {
                                Text("Developer settings")
                                Text("Engine rollout switches and diagnostic exports.")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
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

                // No roaming toggle here, unlike Android. iOS cannot tell an
                // app whether the cellular path is roaming, so a switch of
                // ours would gate nothing and quietly lie. The control that
                // does work is Apple's own, and it is stronger than ours:
                // with Data Roaming off, the modem carries no roaming data at
                // all. Point at it rather than imitating it.
                Section("Advanced") {
                    Text(
                        String(
                            localized: "Roaming data is controlled by iOS. To prevent roaming charges at sea, turn off Data Roaming in Settings, under Cellular, then Cellular Data Options."
                        )
                    )
                    .font(.footnote)
                    .foregroundStyle(.secondary)
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
                        // must not land in the localization catalog. The tap
                        // feedback that replaces it is looked up separately.
                        //
                        // The version line doubles as the door to internal
                        // tools: seven taps turn them on, seven more hide them
                        // again. Deliberately undiscoverable -- a family member
                        // scrolling to the bottom of Settings should never
                        // arrive here by accident, so there is no button, no
                        // label and nothing said for the first three taps.
                        //
                        // All the feedback is this row's own text, swapped in
                        // place at the same size and reverted a moment after
                        // the last tap. Nothing is drawn over it and nothing
                        // below it moves, so the row stays exactly where the
                        // finger already is for the whole run.
                        Text(verbatim: versionRowText)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .onTapGesture { registerUnlockTap() }
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

    /// The version row's text: the version string at rest, tap feedback while a
    /// run is in progress.
    ///
    /// One row, one line of text, one hit target. The alternative -- anything
    /// that floats -- sits at the bottom of the screen, which is exactly where
    /// this row is, and every tap it swallows makes the run longer.
    private var versionRowText: String {
        switch versionRowLabel {
        case .version:
            return versionLabel
        case .countdown(let remaining):
            return String(localized: "\(remaining) more taps…")
        case .unlocked:
            return String(localized: "Developer settings on. Reopen settings.")
        case .hidden:
            return String(localized: "Developer settings hidden.")
        }
    }

    /// One tap on the version line. Silent for the first three; from the fourth
    /// a light haptic tap and a count in the row's own text; and on the seventh
    /// the row says which way it went. The Settings entry itself waits until
    /// Settings is next opened, so nothing moves under the finger.
    private func registerUnlockTap() {
        let tap = unlockTaps.tap(at: Date().timeIntervalSince1970)
        var unlocked = DeveloperSettingsUnlockStore.isUnlocked()
        switch tap {
        case .quiet:
            break
        case .countdown:
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
        case .reached:
            unlocked.toggle()
            DeveloperSettingsUnlockStore.setUnlocked(unlocked)
            UINotificationFeedbackGenerator().notificationOccurred(.success)
        }
        versionRowLabel = developerSettingsLabel(for: tap, unlockedAfterTap: unlocked)
        scheduleLabelRevert()
    }

    /// Puts the version string back a moment after the last tap. Cancelling the
    /// previous task is what keeps a run of taps from leaving a trail of
    /// messages that arrive one after another once the tapping has stopped.
    private func scheduleLabelRevert() {
        labelRevert?.cancel()
        guard versionRowLabel != .version else { return }
        labelRevert = Task { @MainActor in
            try? await Task.sleep(nanoseconds: UInt64(developerSettingsLabelRevert * 1_000_000_000))
            guard !Task.isCancelled else { return }
            versionRowLabel = .version
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
        case .deferredRoaming: return String(localized: "Waiting for non-roaming internet to avoid roaming charges")
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
        case .deferredRoaming: return String(localized: "Waiting for non-roaming internet to avoid roaming charges")
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
