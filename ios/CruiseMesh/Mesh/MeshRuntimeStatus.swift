import Combine
import Foundation

enum MeshRuntimeState: Equatable {
    case stopped
    case starting
    case meshing(nearby: Int)
    case syncingViaRelay
}

/**
 Process-wide mesh runtime status for the app UI.

 `bluetoothAudioConnected` is a separate axis from `state`, matching Android's
 `MeshRuntimeStatus`: the mesh no longer pauses when Bluetooth audio is routed
 to a headset, so it keeps reporting `meshing` while audio plays. The flag only
 lets the UI show an informational banner, so a user knows audio and the mesh
 are sharing the radio.
 */
@MainActor
final class MeshRuntimeStatus: ObservableObject {
    static let shared = MeshRuntimeStatus()

    @Published private(set) var state: MeshRuntimeState = .stopped
    @Published private(set) var bluetoothAudioConnected = false

    func markStopped() {
        state = .stopped
        bluetoothAudioConnected = false
    }
    func markStarting() { state = .starting }
    func markMeshing(nearby: Int) { state = .meshing(nearby: nearby) }
    func markSyncingViaRelay() { state = .syncingViaRelay }
    func setBluetoothAudioConnected(_ connected: Bool) { bluetoothAudioConnected = connected }

    var pillText: String {
        switch state {
        case .stopped: return "Mesh off"
        case .starting: return "Starting…"
        case .meshing(let n): return n > 0 ? "Meshing · \(n) nearby" : "Meshing"
        case .syncingViaRelay: return "Syncing via relay"
        }
    }
}
