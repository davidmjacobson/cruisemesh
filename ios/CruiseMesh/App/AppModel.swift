import Combine
import Foundation
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    // FI3: not `private` so `AppDelegate` (CruiseMeshApp.swift) can check
    // the same persisted preference before `AppModel` itself exists, on a
    // background BLE-restoration relaunch.
    static let meshEnabledKey = "cruisemesh.mesh.enabled"

    let identity: Identity
    @Published var displayName: String
    @Published private(set) var meshEnabled: Bool
    @Published var pendingFriendToken: String?
    @Published var pendingRelayCard: String?
    /// A tapped LAN address link waiting for the person's answer. See
    /// `LanEndpointLinkDecision` for why a link never dials on its own.
    @Published var lanLinkDecision: LanEndpointLinkDecision?

    init() {
        try? BackupService.installPendingRestoreIfNeeded()
        let id = IdentityStore.loadOrCreate()
        UITestConfiguration.prepareFixtures(identity: id)
        self.identity = id
        self.displayName = ProfileStore.loadDisplayName()
        self.pendingFriendToken = nil
        self.pendingRelayCard = nil
        self.lanLinkDecision = nil
        if UITestConfiguration.isEnabled {
            self.meshEnabled = false
        } else if AppDefaults.current.object(forKey: Self.meshEnabledKey) == nil {
            self.meshEnabled = true
        } else {
            self.meshEnabled = AppDefaults.current.bool(forKey: Self.meshEnabledKey)
        }
        if !UITestConfiguration.isEnabled {
            MeshController.shared.configure(identity: id)
        }
    }

    func startMesh() {
        guard TermsAcceptanceStore.isCurrentVersionAccepted() else { return }
        meshEnabled = true
        AppDefaults.current.set(true, forKey: Self.meshEnabledKey)
        guard !UITestConfiguration.isEnabled else { return }
        MessageNotifier.requestPermission()
        MeshController.shared.start()
    }

    func startMeshIfEnabled() {
        guard !UITestConfiguration.isEnabled,
              meshEnabled,
              TermsAcceptanceStore.isCurrentVersionAccepted() else { return }
        MeshController.shared.start()
    }

    func stopMesh() {
        meshEnabled = false
        AppDefaults.current.set(false, forKey: Self.meshEnabledKey)
        guard !UITestConfiguration.isEnabled else { return }
        MeshController.shared.stop()
    }

    /// A LAN address link arrived: decide what it may do and show that.
    /// Nothing is dialed and the mesh is never started from here.
    func handleLanEndpointLink(_ endpoint: LanManualEndpoint) {
        lanLinkDecision = currentLanLinkDecision(endpoint)
    }

    /// The person said yes to the address. Ask again at that moment: the mesh
    /// or the Wi-Fi network may have changed while the alert was open.
    func confirmLanEndpointLink(_ endpoint: LanManualEndpoint) {
        let current = currentLanLinkDecision(endpoint)
        guard case let .confirm(confirmed) = current else {
            // The alert is dismissing itself as this runs; show the reason
            // once it has gone rather than having the dismissal clear it.
            DispatchQueue.main.async { [weak self] in self?.lanLinkDecision = current }
            return
        }
        lanLinkDecision = nil
        LanTransportDiagnostics.shared.queueManualConnection(confirmed)
    }

    private func currentLanLinkDecision(_ endpoint: LanManualEndpoint) -> LanEndpointLinkDecision {
        decideLanEndpointLink(
            endpoint,
            meshRunning: meshEnabled && !UITestConfiguration.isEnabled,
            localEndpoint: LanTransportDiagnostics.shared.snapshot.localEndpoint
        )
    }

    func setAppForeground(_ foreground: Bool) {
        guard !UITestConfiguration.isEnabled else { return }
        MeshController.shared.setAppForeground(foreground)
    }
}
