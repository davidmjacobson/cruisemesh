import Foundation
import os.log

/// Process-wide send router (Android `MeshRouter` parity).
enum MeshRouter {
    private static let state = MeshRouterState()

    /// The shared Rust route state, for `MessageStore.corePlanMeshMeet` alone
    /// (see `MeshRouterState.coreState`). Everything else goes through the
    /// named delegations below.
    static var coreState: CoreMeshRouterState { state.coreState }
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

    static func setLocalUserId(_ userId: Data) {
        state.setLocalUserId(userId)
    }

    static func onConnected(address: String, transport: MeshRouterState.Transport) {
        state.onConnected(address: address, transport: transport)
    }

    /// A link that proved it belongs to one of *this person's own* devices
    /// (`specs/multi-device-v1.md` §10 step 5): a transport for the roster
    /// notice, and never a route.
    ///
    /// Distinct from `onConnected` because "no user id yet" and "never a peer"
    /// are different facts that look alike from here. Core floods every link of
    /// the first kind — that is what makes gossip work before a HELLO lands —
    /// and the device still holding this person's agreement key after a removal
    /// is the device that was removed.
    static func onOwnDeviceConnected(address: String, transport: MeshRouterState.Transport) {
        state.onOwnDeviceConnected(address: address, transport: transport)
    }

    static func onDisconnected(address: String) {
        state.onDisconnected(address: address)
    }

    @discardableResult
    static func onHello(address: String, userId: Data) -> Bool {
        state.onHello(address: address, userId: userId)
    }

    @discardableResult
    static func onHello2(address: String, userId: Data, capabilities: UInt32) -> Bool {
        state.onHello2(address: address, userId: userId, capabilities: capabilities)
    }

    /// Whether `address` advertised the capability bit for this one hidden
    /// spray `kind` -- asked per kind, so a peer that acks four of the five it
    /// knows about keeps the watermark for those four. False for a pre-HELLO2
    /// build, which advertises nothing.
    static func peerAcksHiddenKind(address: String, kind: UInt8) -> Bool {
        state.peerAcksHiddenKind(address: address, kind: kind)
    }

    /// Every hidden spray kind `address` will ack, for the plan builder.
    static func peerAckedHiddenKinds(address: String) -> Data {
        state.peerAckedHiddenKinds(address: address)
    }

    static func hiddenOfferedFor(address: String) -> [Data] {
        state.hiddenOfferedFor(address: address)
    }

    static func recordHiddenOffered(address: String, msgIds: [Data]) {
        state.recordHiddenOffered(address: address, msgIds: msgIds)
    }

    /// Where this link's foreign-carry lane should resume, or whether to sit
    /// this re-digest out because the walk is done and still cooling down.
    static func carriedLaneFor(address: String, nowMs: Int64) -> CoreCarriedLane {
        state.carriedLaneFor(address: address, nowMs: nowMs)
    }

    /// Record how far the carried lane just walked down `address`.
    static func recordCarriedProgress(address: String, next: CoreCarriedCursor?, exhausted: Bool, nowMs: Int64) {
        state.recordCarriedProgress(address: address, next: next, exhausted: exhausted, nowMs: nowMs)
    }

    /// Targeted HELLO drain lane (envelopes for this peer) — G2.
    static func targetedCarriedLaneFor(address: String, nowMs: Int64) -> CoreCarriedLane {
        state.targetedCarriedLaneFor(address: address, nowMs: nowMs)
    }

    static func recordTargetedCarriedProgress(address: String, next: CoreCarriedCursor?, exhausted: Bool, nowMs: Int64) {
        state.recordTargetedCarriedProgress(address: address, next: next, exhausted: exhausted, nowMs: nowMs)
    }

    static func userIdFor(address: String) -> Data? {
        state.userIdFor(address: address)
    }

    static func connectedUserCount() -> Int {
        state.connectedUserCount()
    }

    static func transportFor(address: String) -> MeshRouterState.Transport? {
        state.transportFor(address: address)
    }

    static func routeFor(userId: Data) -> (MeshRouterState.Transport, String)? {
        state.routeFor(userId: userId)
    }

    static func identifiedRoutes() -> [MeshRouterState.IdentifiedRoute] {
        state.identifiedRoutes()
    }

    static func selectedIdentifiedRoutes() -> [MeshRouterState.IdentifiedRoute] {
        state.selectedIdentifiedRoutes()
    }

    static func isSelectedRoute(address: String) -> Bool {
        state.isSelectedRoute(address: address)
    }

    @discardableResult
    static func sendToUserId(userId: Data, frame: Data) -> Bool {
        let plan = transportSendPlan(routes: state.routesFor(userId: userId), frameSize: frame.count)
        var sent = false
        for (transport, address) in plan {
            sent = dispatch(transport: transport, address: address, frame: frame) || sent
        }
        return sent
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
        for (transport, address) in state.relayRoutes(exceptAddress: exceptAddress) {
            if dispatch(transport: transport, address: address, frame: frame) { count += 1 }
        }
        return count
    }

    @discardableResult
    static func relayToAll(frame: Data) -> Int {
        var count = 0
        for (transport, address) in state.relayRoutes() {
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
