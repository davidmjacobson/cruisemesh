import SwiftUI
import UIKit

/// Engine rollout switches and diagnostic exports, and nothing else.
///
/// Reachable on a debug build outright, and on a release build once someone has
/// done the seven-tap run on the version line in Settings. It has to be
/// reachable on release: a TestFlight build is signed for release, and a
/// staged-rollout canary whose switches only exist in a developer's own build
/// can never produce the field evidence it exists to produce.
///
/// Deliberately not a second home for anything a person can already do on a
/// visible screen. Relay URL and token entry lives on the Shore Pass screen,
/// under "Custom relay", which checks the pair against the relay before saving
/// it; this screen used to carry a second copy that saved every keystroke,
/// unchecked.
struct DeveloperSettingsView: View {
    /// Told when this screen hides itself again, so the Settings row it was
    /// reached from can disappear with it.
    var onLock: () -> Void = {}
    @Environment(\.dismiss) private var dismiss

    @State private var useCoreRelayEngine = false
    @State private var relayShadowOn = true
    @State private var useCoreInboundEngine = false
    @State private var useCoreMeetEngine = false
    @State private var exportError: String?
    @State private var shareFile: ShareableFile?

    var body: some View {
        // Sections are separate `some View` builders so the type checker
        // never has to solve the entire Form as one expression (Xcode 26
        // times out on that).
        Form {
            releaseWarningSection
            deliverySection
            diagnosticsSection
            lockAgainSection
        }
        .navigationTitle("Developer settings")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            useCoreRelayEngine = RelayEngineSettings.passEngine() == .core
            relayShadowOn = RelayEngineSettings.shadowEnabled()
            useCoreInboundEngine = InboundEngineSettings.pathEngine() == .core
            useCoreMeetEngine = MeetEngineSettings.meetEngine() == .core
        }
        .sheet(item: $shareFile) { file in
            ActivityShareView(items: file.urls)
        }
    }

    /// On a release build these switches are only here because someone did the
    /// seven-tap run. Say plainly what they do before they are touched. A debug
    /// build skips it -- a developer knows.
    @ViewBuilder
    private var releaseWarningSection: some View {
        if developerSettingsUnlockedOnRelease {
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
        if developerSettingsUnlockedOnRelease {
            Section {
                Button("Hide developer settings", role: .destructive) {
                    DeveloperSettingsUnlockStore.setUnlocked(false)
                    onLock()
                    dismiss()
                }
            }
        }
    }

    /// The four engine rollout switches. Each one is read at the start of the
    /// work it governs, so flipping it takes effect on the next pass, the next
    /// arriving frame, or the next nearby catch-up rather than needing a
    /// restart.
    @ViewBuilder
    private var deliverySection: some View {
        Section("Delivery engines") {
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

            Toggle("Rebuilt nearby exchange", isOn: Binding(
                get: { useCoreMeetEngine },
                set: {
                    useCoreMeetEngine = $0
                    MeetEngineSettings.setMeetEngine($0 ? .core : .legacy)
                }
            ))
            Text("Handles catching up with a nearby phone using the rebuilt shared engine. The old path stays the default; this is for testing only.")
                .font(.caption)
                .foregroundStyle(.secondary)
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
                    exportError = "No diagnostics captured this session yet"
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
                    exportError = "No field metrics captured yet"
                }
            } label: {
                Label("Export field metrics", systemImage: "square.and.arrow.up")
            }
            Text("Exports a CSV of delivery timings and the transports messages used, for cruise-test analysis. Metadata only — no message content or contact names.")
                .font(.caption)
                .foregroundStyle(.secondary)

            if let exportError {
                Text(exportError).font(.caption).foregroundStyle(.red)
            }
        }
    }
}
