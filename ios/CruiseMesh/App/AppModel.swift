import Combine
import Foundation
import SwiftUI

@MainActor
final class AppModel: ObservableObject {
    let identity: Identity
    @Published var displayName: String
    @Published var meshStarted = false

    init() {
        let id = IdentityStore.loadOrCreate()
        self.identity = id
        self.displayName = ProfileStore.loadDisplayName()
        MeshController.shared.configure(identity: id)
        MessageNotifier.requestPermission()
    }

    func startMesh() {
        MeshController.shared.start()
        meshStarted = true
    }
}
