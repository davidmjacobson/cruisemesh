import Combine
import Foundation
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    private static let meshEnabledKey = "cruisemesh.mesh.enabled"

    let identity: Identity
    @Published var displayName: String
    @Published private(set) var meshEnabled: Bool

    init() {
        let id = IdentityStore.loadOrCreate()
        self.identity = id
        self.displayName = ProfileStore.loadDisplayName()
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
}
