import Combine
import Foundation

enum MeshRuntimeState: Equatable {
    case stopped
    case starting
    case meshing(nearby: Int)
    case pausedForBluetoothAudio
    case syncingViaRelay
}

@MainActor
final class MeshRuntimeStatus: ObservableObject {
    static let shared = MeshRuntimeStatus()

    @Published private(set) var state: MeshRuntimeState = .stopped

    func markStopped() { state = .stopped }
    func markStarting() { state = .starting }
    func markMeshing(nearby: Int) { state = .meshing(nearby: nearby) }
    func markPausedForBluetoothAudio() { state = .pausedForBluetoothAudio }
    func markSyncingViaRelay() { state = .syncingViaRelay }

    var pillText: String {
        switch state {
        case .stopped: return "Mesh off"
        case .starting: return "Starting…"
        case .meshing(let n): return n > 0 ? "Meshing · \(n) nearby" : "Meshing"
        case .pausedForBluetoothAudio: return "Paused — Bluetooth audio"
        case .syncingViaRelay: return "Syncing via relay"
        }
    }
}
