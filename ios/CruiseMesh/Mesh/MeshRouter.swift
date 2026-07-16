import Foundation
import os.log

/// Process-wide send router (Android `MeshRouter` parity).
enum MeshRouter {
    private static let state = MeshRouterState()
    private static let log = Logger(subsystem: "com.cruisemesh", category: "MeshRouter")
    private static var centralSend: ((String, Data) -> Void)?
    private static var peripheralSend: ((String, Data) -> Void)?
    private static var lanSend: ((String, Data) -> Void)?
    private static let lock = NSLock()

    static func registerCentral(send: @escaping (String, Data) -> Void) {
        lock.lock(); defer { lock.unlock() }
        centralSend = send
    }

    static func registerPeripheral(send: @escaping (String, Data) -> Void) {
        lock.lock(); defer { lock.unlock() }
        peripheralSend = send
    }

    static func registerLan(send: @escaping (String, Data) -> Void) {
        lock.lock(); defer { lock.unlock() }
        lanSend = send
    }

    static func unregisterCentral() {
        lock.lock(); defer { lock.unlock() }
        centralSend = nil
    }

    static func unregisterPeripheral() {
        lock.lock(); defer { lock.unlock() }
        peripheralSend = nil
    }

    static func unregisterLan() {
        lock.lock(); defer { lock.unlock() }
        lanSend = nil
    }

    static func reset() { state.clear() }

    static func resetBle() {
        state.clear(transports: [.central, .peripheral])
    }

    static func onConnected(address: String, transport: MeshRouterState.Transport) {
        state.onConnected(address: address, transport: transport)
    }

    static func onDisconnected(address: String) {
        state.onDisconnected(address: address)
    }

    @discardableResult
    static func onHello(address: String, userId: Data) -> Bool {
        state.onHello(address: address, userId: userId)
    }

    static func userIdFor(address: String) -> Data? {
        state.userIdFor(address: address)
    }

    static func connectedUserCount() -> Int {
        state.connectedUserCount()
    }

    @discardableResult
    static func sendToUserId(userId: Data, frame: Data) -> Bool {
        guard let (transport, address) = state.routeFor(userId: userId) else { return false }
        return dispatch(transport: transport, address: address, frame: frame)
    }

    @discardableResult
    static func sendToAddress(address: String, frame: Data) -> Bool {
        guard let transport = state.transportFor(address: address) else {
            log.warning("sendToAddress: \(address, privacy: .public) not connected")
            return false
        }
        return dispatch(transport: transport, address: address, frame: frame)
    }

    @discardableResult
    static func relayToAllExcept(_ exceptAddress: String, frame: Data) -> Int {
        var count = 0
        for (transport, address) in state.connectedRoutes() where address != exceptAddress {
            if dispatch(transport: transport, address: address, frame: frame) { count += 1 }
        }
        return count
    }

    @discardableResult
    static func relayToAll(frame: Data) -> Int {
        var count = 0
        for (transport, address) in state.connectedRoutes() {
            if dispatch(transport: transport, address: address, frame: frame) { count += 1 }
        }
        return count
    }

    private static func dispatch(transport: MeshRouterState.Transport, address: String, frame: Data) -> Bool {
        lock.lock()
        let send: ((String, Data) -> Void)?
        switch transport {
        case .central: send = centralSend
        case .peripheral: send = peripheralSend
        case .lan: send = lanSend
        }
        lock.unlock()
        guard let send else { return false }
        send(address, frame)
        return true
    }
}
