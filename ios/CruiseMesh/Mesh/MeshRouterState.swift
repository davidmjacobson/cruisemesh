import Foundation

/// iOS-shaped adapter around the shared, thread-safe Rust route state.
final class MeshRouterState {
    enum Transport: String {
        case central
        case peripheral
        case lan

        var routePriority: Int { self == .lan ? 10 : 0 }
    }

    struct IdentifiedRoute: Equatable {
        let transport: Transport
        let address: String
        let userId: Data
    }

    private let core = CoreMeshRouterState()

    func onConnected(address: String, transport: Transport) {
        core.onConnected(address: address, transport: transport.core)
    }

    func onDisconnected(address: String) { core.onDisconnected(address: address) }

    @discardableResult
    func onHello(address: String, userId: Data) -> Bool {
        core.onHello(address: address, userId: userId)
    }

    @discardableResult
    func onHello2(address: String, userId: Data, capabilities: UInt32) -> Bool {
        core.onHello2(address: address, userId: userId, capabilities: capabilities)
    }

    func peerAcksHiddenKinds(address: String) -> Bool { core.peerAcksHiddenKinds(address: address) }
    func hiddenOfferedFor(address: String) -> [Data] { core.hiddenOfferedFor(address: address) }
    func recordHiddenOffered(address: String, msgIds: [Data]) {
        core.recordHiddenOffered(address: address, msgIds: msgIds)
    }

    func userIdFor(address: String) -> Data? { core.userIdFor(address: address) }
    func transportFor(address: String) -> Transport? { core.transportFor(address: address)?.platform }
    func connectedRoutes() -> [(Transport, String)] { core.connectedRoutes().map { ($0.transport.platform, $0.address) } }
    func identifiedRoutes() -> [IdentifiedRoute] {
        core.identifiedRoutes().map { IdentifiedRoute(transport: $0.transport.platform, address: $0.address, userId: $0.userId) }
    }
    func connectedUserCount() -> Int { Int(core.connectedUserCount()) }
    func routeFor(userId: Data) -> (Transport, String)? {
        core.routeFor(userId: userId).map { ($0.transport.platform, $0.address) }
    }
    func routesFor(userId: Data) -> [(Transport, String)] {
        core.routesFor(userId: userId).map { ($0.transport.platform, $0.address) }
    }
    func clear(transports: Set<Transport>) { core.clearTransports(transports: transports.map(\.core)) }
    func clear() { core.clear() }
}

private extension MeshRouterState.Transport {
    var core: CoreTransport {
        switch self {
        case .central: return .central
        case .peripheral: return .peripheral
        case .lan: return .lan
        }
    }
}

private extension CoreTransport {
    var platform: MeshRouterState.Transport {
        switch self {
        case .central: return .central
        case .peripheral: return .peripheral
        case .lan: return .lan
        }
    }
}

func transportSendPlan(
    routes: [(MeshRouterState.Transport, String)],
    frameSize: Int
) -> [(MeshRouterState.Transport, String)] {
    coreTransportSendPlan(
        routes: routes.map { CoreTransportRoute(transport: $0.0.core, address: $0.1) },
        frameSize: UInt32(frameSize)
    ).map { ($0.transport.platform, $0.address) }
}
