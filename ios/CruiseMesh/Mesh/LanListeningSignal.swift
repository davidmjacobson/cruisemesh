import Combine
import Foundation

/**
 Whether the local Wi-Fi transport is holding a listening socket, and nothing
 else about it.

 `LanTransportDiagnostics.snapshot` changes on every peer, every probe, and
 every sweep tick. A view that observed the whole snapshot to learn one boolean
 would redraw at LAN-event rates for a flag that flips when the mesh starts and
 stops -- which is the cost the home screen cannot afford now that the status
 pill needs the local Wi-Fi path to reach the core's classification. This maps
 first and drops duplicates, so a subscriber is woken only when the answer
 actually changes.

 Only the *existence* of the endpoint is read. The endpoint itself never leaves
 this file: addresses and network names stay off every screen. Mirrors the
 `LanTransportDiagnostics.state.map { it.localEndpoint != null }
 .distinctUntilChanged()` flow on the Android home screen.
 */
@MainActor
final class LanListeningSignal: ObservableObject {
    static let shared = LanListeningSignal()

    @Published private(set) var isListening = false

    private var cancellable: AnyCancellable?

    private init() {
        // `LanTransportDiagnostics` publishes on the main queue already (see
        // its `publish`), so this lands on the main actor without a hop.
        cancellable = LanTransportDiagnostics.shared.$snapshot
            .map { $0.localEndpoint != nil }
            .removeDuplicates()
            .sink { [weak self] listening in
                self?.isListening = listening
            }
    }
}
