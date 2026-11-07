import Foundation

/// The callbacks `BleTransport` reports into. Handed over as one value in
/// `BleTransport.start(handlers:)` rather than assigned property by property,
/// so they reach the BLE queue through that queue (no cross-queue write) and
/// can never be half-installed.
struct BleTransportHandlers {
    var onFrame: (String, Data) -> Void
    var onCentralConnected: (String) -> Void
    var onCentralDisconnected: (String) -> Void
    var onPeripheralSubscribed: (String) -> Void
    var onPeripheralUnsubscribed: (String) -> Void
}

enum BleTransportEvent: Equatable {
    case frame(address: String, frame: Data)
    case centralConnected(address: String)
    case centralDisconnected(address: String)
    case peripheralSubscribed(address: String)
    case peripheralUnsubscribed(address: String)
}

/// Holds `BleTransport` events until someone is listening.
///
/// On a background Bluetooth relaunch the system delivers
/// `willRestoreState` to the transport's managers as soon as they exist,
/// which is inside `MeshController.shared`'s initializer -- well before
/// `MeshController` has started and handed over its handlers. A restored,
/// already-subscribed central (or a restored connection that finishes its
/// rediscovery quickly) used to be reported into nothing: the route was never
/// registered, no Hello went out, and the link stayed dead for the life of the
/// process.
///
/// So until the first `open`, events are kept in arrival order and replayed
/// when the handlers arrive. Once the transport is stopped on purpose, events
/// are dropped instead: anything that trickles in after `stop()` belongs to a
/// session that is over, and replaying it into the next one would register
/// routes for links that no longer exist.
///
/// Not thread-safe by itself: `BleTransport` only touches it on its own queue.
final class BleEventGate {
    /// Frames are the only events whose count is not bounded by the number of
    /// nearby peers, so they alone are capped while waiting. Connection events
    /// are always kept -- dropping one would desynchronize the route table.
    static let maxBufferedFrames = 64

    private enum State {
        case awaitingStart
        case open(BleTransportHandlers)
        case stopped
    }

    private var state: State = .awaitingStart
    private var buffered: [BleTransportEvent] = []
    private var bufferedFrameCount = 0

    var bufferedEvents: [BleTransportEvent] { buffered }

    func emit(_ event: BleTransportEvent) {
        switch state {
        case .open(let handlers):
            Self.dispatch(event, to: handlers)
        case .stopped:
            return
        case .awaitingStart:
            if case .frame = event {
                guard bufferedFrameCount < Self.maxBufferedFrames else { return }
                bufferedFrameCount += 1
            }
            buffered.append(event)
        }
    }

    /// Installs `handlers` and replays everything held since launch, in order.
    func open(_ handlers: BleTransportHandlers) {
        state = .open(handlers)
        let replay = buffered
        buffered.removeAll()
        bufferedFrameCount = 0
        for event in replay {
            Self.dispatch(event, to: handlers)
        }
    }

    /// The transport was stopped on purpose: forget the handlers and anything
    /// still held, and drop whatever arrives until the next `open`.
    func close() {
        state = .stopped
        buffered.removeAll()
        bufferedFrameCount = 0
    }

    private static func dispatch(_ event: BleTransportEvent, to handlers: BleTransportHandlers) {
        switch event {
        case .frame(let address, let frame):
            handlers.onFrame(address, frame)
        case .centralConnected(let address):
            handlers.onCentralConnected(address)
        case .centralDisconnected(let address):
            handlers.onCentralDisconnected(address)
        case .peripheralSubscribed(let address):
            handlers.onPeripheralSubscribed(address)
        case .peripheralUnsubscribed(let address):
            handlers.onPeripheralUnsubscribed(address)
        }
    }
}
