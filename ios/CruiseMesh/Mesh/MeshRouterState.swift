import Foundation

final class MeshRouterState {
    enum Transport: String {
        case central
        case peripheral
        case lan

        var routePriority: Int {
            self == .lan ? 10 : 0
        }
    }

    private struct Peer {
        var transport: Transport
        var userId: Data?
    }

    struct IdentifiedRoute: Equatable {
        let transport: Transport
        let address: String
        let userId: Data
    }

    private var peersByAddress: [String: Peer] = [:]
    private let lock = NSLock()

    func onConnected(address: String, transport: Transport) {
        lock.lock(); defer { lock.unlock() }
        peersByAddress[address] = Peer(transport: transport, userId: nil)
    }

    func onDisconnected(address: String) {
        lock.lock(); defer { lock.unlock() }
        peersByAddress.removeValue(forKey: address)
    }

    @discardableResult
    func onHello(address: String, userId: Data) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard var peer = peersByAddress[address] else { return false }
        if let existing = peer.userId, existing != userId { return false }
        peer.userId = userId
        peersByAddress[address] = peer
        return true
    }

    func userIdFor(address: String) -> Data? {
        lock.lock(); defer { lock.unlock() }
        return peersByAddress[address]?.userId
    }

    func transportFor(address: String) -> Transport? {
        lock.lock(); defer { lock.unlock() }
        return peersByAddress[address]?.transport
    }

    func connectedRoutes() -> [(Transport, String)] {
        lock.lock(); defer { lock.unlock() }
        return peersByAddress.map { ($0.value.transport, $0.key) }
    }

    func identifiedRoutes() -> [IdentifiedRoute] {
        lock.lock(); defer { lock.unlock() }
        return peersByAddress.compactMap { address, peer in
            guard let userId = peer.userId else { return nil }
            return IdentifiedRoute(transport: peer.transport, address: address, userId: userId)
        }
    }

    func connectedUserCount() -> Int {
        lock.lock(); defer { lock.unlock() }
        var seen = Set<Data>()
        for peer in peersByAddress.values {
            if let id = peer.userId { seen.insert(id) }
        }
        return seen.count
    }

    func routeFor(userId: Data) -> (Transport, String)? {
        routesFor(userId: userId).first
    }

    func routesFor(userId: Data) -> [(Transport, String)] {
        lock.lock(); defer { lock.unlock() }
        return peersByAddress.compactMap { address, peer in
            guard peer.userId == userId else { return nil }
            return (peer.transport, address)
        }.sorted { $0.0.routePriority > $1.0.routePriority }
    }

    func clear(transports: Set<Transport>) {
        lock.lock(); defer { lock.unlock() }
        peersByAddress = peersByAddress.filter { !transports.contains($0.value.transport) }
    }

    func clear() {
        lock.lock(); defer { lock.unlock() }
        peersByAddress.removeAll()
    }
}

/// Small control/text frames race over LAN and one BLE path. Larger frames use
/// LAN only when it is available, avoiding expensive duplicate attachment data.
func transportSendPlan(
    routes: [(MeshRouterState.Transport, String)],
    frameSize: Int
) -> [(MeshRouterState.Transport, String)] {
    guard let lan = routes.first(where: { $0.0 == .lan }) else {
        return Array(routes.prefix(1))
    }
    guard frameSize <= 8 * 1_024,
          let ble = routes.first(where: { $0.0 != .lan }) else {
        return [lan]
    }
    return [lan, ble]
}
