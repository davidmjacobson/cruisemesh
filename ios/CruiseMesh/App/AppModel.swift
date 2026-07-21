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

    init() {
        try? BackupService.installPendingRestoreIfNeeded()
        let id = IdentityStore.loadOrCreate()
        self.identity = id
        self.displayName = ProfileStore.loadDisplayName()
        self.pendingFriendToken = nil
        if UserDefaults.standard.object(forKey: Self.meshEnabledKey) == nil {
            self.meshEnabled = true
        } else {
            self.meshEnabled = UserDefaults.standard.bool(forKey: Self.meshEnabledKey)
        }
        MeshController.shared.configure(identity: id)
    }

    func startMesh() {
        meshEnabled = true
        UserDefaults.standard.set(true, forKey: Self.meshEnabledKey)
        MessageNotifier.requestPermission()
        MeshController.shared.start()
    }

    func startMeshIfEnabled() {
        guard meshEnabled else { return }
        MeshController.shared.start()
    }

    func stopMesh() {
        meshEnabled = false
        UserDefaults.standard.set(false, forKey: Self.meshEnabledKey)
        MeshController.shared.stop()
    }

    func setAppForeground(_ foreground: Bool) {
        MeshController.shared.setAppForeground(foreground)
    }
}
