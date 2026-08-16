import CoreBluetooth
import SwiftUI
import UIKit
import UserNotifications

/**
 Gathers the facts behind the checklist and hands the core's answer to the
 views.

 Deliberately re-read rather than observed: every fact here changes because
 somebody left the app and came back (a permission alert, a trip to Settings,
 the friend sheet), so `refresh()` on appear and on becoming active covers
 every path without a single subscription. The one exception is the
 notification grant, which iOS only answers through a callback.
 */
@MainActor
final class SailChecklistModel: ObservableObject {
    @Published private(set) var facts = SailChecklistFacts.unknown

    /// False until the first full read -- including the async notification
    /// callback -- has landed. The home card gates on this so a cold launch
    /// never flashes "0 of 5 done" at a family that finished weeks ago.
    @Published private(set) var hasLoaded = false

    /// Nonisolated so `@StateObject private var model = SailChecklistModel()`
    /// needs no actor hop to build the view.
    nonisolated init() {}

    var report: CoreSailChecklistReport { SailChecklistInputs.report(for: facts) }

    func refresh() {
        let contactCount = ((try? AppStore.get().listContacts()) ?? []).count
        facts = SailChecklistFacts(
            contactCount: contactCount,
            shorePassConfigured: RelayConfigStore.load() != nil,
            // Read statically rather than through `BluetoothAccess.shared`:
            // only the recorded decision matters here, and this screen is
            // reachable from Settings on a phone whose mesh is stopped.
            bluetooth: UITestConfiguration.isEnabled ? .allowedAlways : CBCentralManager.authorization,
            // Kept from the previous read until the callback below lands, so a
            // refresh never flickers a granted permission back to undecided.
            notifications: UITestConfiguration.isEnabled ? .authorized : facts.notifications,
            offlineDeliverySeen: OfflineDeliverySeenStore.hasSeen(),
            backupCreated: BackupCreatedStore.hasCreated()
        )
        guard !UITestConfiguration.isEnabled else {
            hasLoaded = true
            return
        }
        refreshNotifications()
    }

    private func refreshNotifications() {
        UNUserNotificationCenter.current().getNotificationSettings { [weak self] settings in
            let status = settings.authorizationStatus
            Task { @MainActor in
                guard let self else { return }
                if self.facts.notifications != status {
                    self.facts = self.facts.withNotifications(status)
                }
                self.hasLoaded = true
            }
        }
    }
}

/**
 The "Before you sail" checklist.

 A list, not a wizard: nothing here blocks, every step is reachable in any
 order, and a step ticks itself from what the app already knows rather than
 from a button somebody pressed. The order comes from the core and is not
 rearranged here -- the Shore Pass sits first because friend codes carry its
 delivery details, so a pass added afterwards leaves every code already traded
 out of date.
 */
struct SailChecklistView: View {
    @ObservedObject var appModel: AppModel
    /// True only when presented as a sheet: the sheet call site supplies the
    /// NavigationStack and needs a Close button; the Settings push supplies
    /// neither, and the system back button is the only chrome it wants.
    var isModal: Bool = false

    @StateObject private var model = SailChecklistModel()

    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase

    @State private var showShorePass = false
    @State private var showFriends = false

    var body: some View {
        // Derived once per body evaluation from the facts the model holds; the
        // policy behind it is the core's.
        let report = model.report
        // No NavigationStack of its own: Settings pushes this view onto its
        // stack, and the chat-list sheet wraps it in one. Owning a nested
        // stack here doubled the navigation bar on the pushed copy.
        return List {
                Section {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(SailChecklistCopy.intro(ready: report.ready))
                            .font(.subheadline)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(SailChecklistCopy.progress(
                            done: report.doneCount,
                            total: report.totalCount
                        ))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 2)
                }
                Section {
                    ForEach(report.items, id: \.id) { item in
                        row(for: item, report: report)
                    }
                }
            }
        .navigationTitle("Before you sail")
        .toolbar {
            if isModal {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .onAppear { model.refresh() }
        .onChange(of: scenePhase) { phase in
            // A permission alert and a trip to Settings both take the scene
            // out of `.active` and bring it back. Re-reading here is what
            // makes a granted permission tick without leaving the screen.
            guard phase == .active else { return }
            model.refresh()
        }
        .sheet(isPresented: $showShorePass, onDismiss: { model.refresh() }) {
            NavigationStack {
                ShorePassView(initialCard: nil, appModel: appModel)
            }
        }
        .sheet(isPresented: $showFriends, onDismiss: { model.refresh() }) {
            FriendsView(identity: appModel.identity, appModel: appModel) {
                showFriends = false
            }
        }
        .accessibilityIdentifier("screen.sail-checklist")
    }

    /**
     One step.

     The permissions step is the only one that is not itself a control: its
     grants open different system screens, so each is its own row underneath.
     The offline test has nothing to open at all -- it is a thing two people do
     with two phones, and a button that opened a screen about it would be
     pretending otherwise.
     */
    @ViewBuilder
    private func row(for item: CoreSailChecklistItem, report: CoreSailChecklistReport) -> some View {
        switch item.id {
        case .shorePass:
            Button { showShorePass = true } label: { stepLabel(item) }
                .buttonStyle(.plain)
        case .addFamily:
            Button { showFriends = true } label: { stepLabel(item) }
                .buttonStyle(.plain)
        case .permissions:
            VStack(alignment: .leading, spacing: 12) {
                stepLabel(item)
                // Rendered straight off the core's list: it has already dropped
                // the grants this platform does not have, and a row added back
                // here would be one iOS can never satisfy.
                ForEach(report.permissions, id: \.permission) { permissionRow in
                    Button {
                        open(permissionRow.permission)
                    } label: {
                        permissionLabel(permissionRow)
                    }
                    .buttonStyle(.plain)
                    .padding(.leading, 32)
                }
            }
        case .offlineTest:
            stepLabel(item)
        case .backup:
            NavigationLink { BackupExportView() } label: { stepLabel(item) }
        }
    }

    private func stepLabel(_ item: CoreSailChecklistItem) -> some View {
        HStack(alignment: .top, spacing: 12) {
            SailStatusIcon(done: item.done)
            VStack(alignment: .leading, spacing: 3) {
                Text(SailChecklistCopy.title(item.id))
                Text(SailChecklistCopy.subtitle(
                    item.id,
                    contactCount: model.facts.contactCount,
                    done: item.done
                ))
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
    }

    private func permissionLabel(_ permissionRow: CoreSailPermissionRow) -> some View {
        HStack(alignment: .top, spacing: 10) {
            SailStatusIcon(done: permissionRow.granted)
            VStack(alignment: .leading, spacing: 2) {
                Text(SailChecklistCopy.permissionTitle(permissionRow.permission))
                    .font(.subheadline)
                Text(SailChecklistCopy.permissionStatus(granted: permissionRow.granted))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
    }

    private func open(_ permission: CoreSailPermission) {
        switch permission {
        case .notifications:
            // An undecided permission still has a system prompt behind it. A
            // refused one does not -- iOS never asks twice -- so the only place
            // left to answer differently is this app's page in Settings.
            if OnboardingPermissions.areNotificationsUndecided(model.facts.notifications) {
                MessageNotifier.requestPermission()
            } else {
                openSailSystemSettings()
            }
        case .bluetooth, .batteryOptimization:
            // The Bluetooth prompt belongs to the manager the app creates at
            // launch, so by the time anyone reads this screen the question has
            // been asked and Settings is where the answer lives.
            openSailSystemSettings()
        }
        model.refresh()
    }
}

/// The tick beside a step. An empty circle rather than a cross: nothing here
/// is a failure, only something not done yet.
private struct SailStatusIcon: View {
    let done: Bool

    var body: some View {
        Image(systemName: done ? "checkmark.circle.fill" : "circle")
            .foregroundStyle(done ? Color.green : Color.secondary)
            .accessibilityLabel(SailChecklistCopy.statusLabel(done: done))
    }
}

/**
 The home-screen entry point.

 Dismissible, and gone on its own once the required steps are done, so it
 cannot become a permanent fixture of the chat list. The checklist itself stays
 in Settings either way -- dismissing the card puts the screen away, it does not
 throw it out.
 */
struct SailChecklistCardView: View {
    let report: CoreSailChecklistReport
    let onOpen: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "checklist")
                .foregroundStyle(Color.accentColor)
            VStack(alignment: .leading, spacing: 3) {
                Text("Before you sail")
                    .font(.subheadline.weight(.semibold))
                Text(SailChecklistCopy.progress(
                    done: report.doneCount,
                    total: report.totalCount
                ))
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Button("Dismiss") { onDismiss() }
                .font(.caption.weight(.semibold))
                .accessibilityIdentifier("home.dismiss-sail-checklist")
        }
        .padding(10)
        .background(Color.accentColor.opacity(0.12))
        .contentShape(Rectangle())
        .onTapGesture { onOpen() }
        .accessibilityIdentifier("home.sail-checklist-card")
    }
}

/// Same destination as `BluetoothAccess.openSystemSettings()`, kept as its own
/// function here since a notification grant is not a Bluetooth concern.
private func openSailSystemSettings() {
    guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
    UIApplication.shared.open(url)
}
