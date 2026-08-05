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

    init() {
        try? BackupService.installPendingRestoreIfNeeded()
        let id = IdentityStore.loadOrCreate()
        UITestConfiguration.prepareFixtures(identity: id)
        self.identity = id
        self.displayName = ProfileStore.loadDisplayName()
        self.pendingFriendToken = nil
        self.pendingRelayCard = nil
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

    func setAppForeground(_ foreground: Bool) {
        guard !UITestConfiguration.isEnabled else { return }
        MeshController.shared.setAppForeground(foreground)
    }
}
