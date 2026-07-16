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

    func connectedUserCount() -> Int {
        lock.lock(); defer { lock.unlock() }
        var seen = Set<Data>()
        for peer in peersByAddress.values {
            if let id = peer.userId { seen.insert(id) }
        }
        return seen.count
    }

    func routeFor(userId: Data) -> (Transport, String)? {
        lock.lock(); defer { lock.unlock() }
        var best: (Transport, String)?
        for (address, peer) in peersByAddress {
            guard let known = peer.userId, known == userId else { continue }
            if best == nil || peer.transport.routePriority > best!.0.routePriority {
                best = (peer.transport, address)
            }
        }
        return best
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
