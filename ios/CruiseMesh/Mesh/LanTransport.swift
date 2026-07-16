import Foundation
import Network
import os.log

/// Opportunistic same-LAN transport using Bonjour, TCP, and the shared
/// Rust-backed Noise XX session. Only accepted contacts become mesh links.
final class LanTransport {
    typealias TrustedPeerLookup = (Data) -> Data?

    var onNetworkReady: ((LanManualEndpoint, Data, String?) -> Void)?
    var onAuthenticated: ((String, Data) -> Void)?
    var onDisconnected: ((String) -> Void)?
    var onFrame: ((String, Data) -> Void)?

    private let log = Logger(subsystem: "com.cruisemesh", category: "LanTransport")
    private let queue = DispatchQueue(label: "com.cruisemesh.lan", qos: .utility)
    private let identity: Identity
    private let trustedPeerForStaticKey: TrustedPeerLookup
    private let diagnostics = LanTransportDiagnostics.shared
    private let instanceToken: Data
    private let instanceTokenString: String

    private var started = false
    private var listener: NWListener?
    private var browser: NWBrowser?
    private var connections: [String: LanConnection] = [:]
    private var discoveredEndpoints: [String: NWEndpoint] = [:]
    private var bonjourServiceKeys = Set<String>()
    private var outboundAddresses: [String: String] = [:]
    private var reconnectAttempts: [String: Int] = [:]
    private var scanGeneration: UUID?
    private var scanCandidates: [String] = []
    private var scanConnections: [UUID: NWConnection] = [:]

    init(identity: Identity, trustedPeerForStaticKey: @escaping TrustedPeerLookup) {
        self.identity = identity
        self.trustedPeerForStaticKey = trustedPeerForStaticKey
        var uuid = UUID().uuid
        let token = withUnsafeBytes(of: &uuid) { Data($0.prefix(8)) }
        instanceToken = token
        instanceTokenString = token.map { String(format: "%02x", $0) }.joined()
    }

    func start() {
        queue.async { [weak self] in
            guard let self, !started else { return }
            started = true
            startListener(preferDefaultPort: true)
            startBrowser()
        }
    }

    func stop() {
        // Retain the transport until asynchronous Network.framework teardown
        // completes; MeshController releases its reference immediately.
        queue.async { [self] in
            guard started else { return }
            started = false
            browser?.cancel()
            browser = nil
            listener?.cancel()
            listener = nil
            scanGeneration = nil
            scanCandidates.removeAll()
            scanConnections.values.forEach { $0.cancel() }
            scanConnections.removeAll()
            discoveredEndpoints.removeAll()
            bonjourServiceKeys.removeAll()
            outboundAddresses.removeAll()
            reconnectAttempts.removeAll()
            let active = Array(connections.values)
            connections.removeAll()
            active.forEach { $0.close(notifyOwner: false) }
            diagnostics.waitingForWifi()
        }
    }

    func sendFrame(address: String, frame: Data) {
        queue.async { [weak self] in
            guard let link = self?.connections[address] else { return }
            link.sendFrame(frame)
            self?.diagnostics.frameSent()
        }
    }

    func connect(_ endpoint: LanManualEndpoint, remoteInstanceToken: Data? = nil, manual: Bool = false) {
        queue.async { [weak self] in
            guard let self, started else { return }
            if let remoteInstanceToken,
               !shouldInitiateLanConnection(
                    localToken: instanceTokenString,
                    remoteToken: remoteInstanceToken.map { String(format: "%02x", $0) }.joined()
               ) {
                return
            }
            let key = "endpoint:\(endpoint.display)"
            let networkEndpoint = NWEndpoint.hostPort(
                host: NWEndpoint.Host(endpoint.host),
                port: NWEndpoint.Port(rawValue: endpoint.port) ?? .any
            )
            discoveredEndpoints[key] = networkEndpoint
            if manual { reconnectAttempts[key] = 0 }
            diagnostics.discovered(endpoint.display)
            connect(to: networkEndpoint, serviceKey: key)
        }
    }

    func closeConnection(address: String) {
        queue.async { [weak self] in
            self?.connections[address]?.close()
        }
    }

    func startSubnetScan() -> String? {
        guard let localAddress = localWifiIPv4Address() else {
            return "Connect this phone to Wi-Fi before searching the local subnet"
        }
        let candidates = subnet24Hosts(localAddress: localAddress)
        guard !candidates.isEmpty else { return "CruiseMesh could not determine the local /24 network" }
        queue.async { [weak self] in
            guard let self, started else { return }
            scanGeneration = UUID()
            scanCandidates = candidates
            scanConnections.values.forEach { $0.cancel() }
            scanConnections.removeAll()
            diagnostics.scanStarted(total: candidates.count)
            for _ in 0..<Self.scanConcurrency {
                startNextScanCandidate(generation: scanGeneration!)
            }
        }
        return nil
    }

    private func startListener(preferDefaultPort: Bool) {
        guard started else { return }
        do {
            let parameters = lanParameters()
            let port = preferDefaultPort
                ? (NWEndpoint.Port(rawValue: lanDefaultTcpPort()) ?? .any)
                : .any
            let newListener = try NWListener(using: parameters, on: port)
            listener = newListener
            let txt = NetService.data(fromTXTRecord: [
                "v": Data("1".utf8),
                "i": Data(instanceTokenString.utf8),
            ])
            var service = NWListener.Service(
                name: instanceTokenString,
                type: appleLanServiceType(),
                domain: nil,
                txtRecord: txt
            )
            service.noAutoRename = true
            newListener.service = service
            newListener.newConnectionHandler = { [weak self] connection in
                self?.queue.async {
                    self?.accept(connection)
                }
            }
            newListener.stateUpdateHandler = { [weak self, weak newListener] state in
                self?.queue.async {
                    guard let self, let newListener, self.listener === newListener else { return }
                    self.handleListenerState(
                        state,
                        listener: newListener,
                        usedDefaultPort: preferDefaultPort
                    )
                }
            }
            newListener.start(queue: queue)
        } catch {
            if preferDefaultPort, isAddressInUse(error) {
                log.warning("TCP \(lanDefaultTcpPort()) is occupied; using an advertised fallback port")
                startListener(preferDefaultPort: false)
            } else {
                log.warning("Unable to start LAN listener: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    private func handleListenerState(
        _ state: NWListener.State,
        listener failedListener: NWListener,
        usedDefaultPort: Bool
    ) {
        switch state {
        case .ready:
            if let port = failedListener.port {
                log.info("Listening for CruiseMesh LAN peers on TCP \(port.rawValue)")
                let endpoint = localWifiIPv4Address().map {
                    LanManualEndpoint(host: $0, port: port.rawValue)
                }
                diagnostics.listening(localEndpoint: endpoint?.display)
                if let endpoint {
                    onNetworkReady?(endpoint, instanceToken, lanNetworkId(ipv4Address: endpoint.host))
                }
            }
        case .failed(let error):
            failedListener.cancel()
            if listener === failedListener {
                listener = nil
            }
            if started, usedDefaultPort, isAddressInUse(error) {
                log.warning("TCP \(lanDefaultTcpPort()) is occupied; using an advertised fallback port")
                startListener(preferDefaultPort: false)
            } else {
                log.warning("LAN listener failed: \(String(describing: error), privacy: .public)")
            }
        case .waiting(let error):
            log.debug("LAN listener waiting: \(String(describing: error), privacy: .public)")
        default:
            break
        }
    }

    private func startBrowser() {
        let newBrowser = NWBrowser(
            for: .bonjour(type: appleLanServiceType(), domain: nil),
            using: lanParameters()
        )
        browser = newBrowser
        newBrowser.browseResultsChangedHandler = { [weak self] results, _ in
            self?.queue.async {
                self?.updateDiscoveredServices(results)
            }
        }
        newBrowser.stateUpdateHandler = { [weak self] state in
            if case .failed(let error) = state {
                self?.log.warning(
                    "LAN discovery failed: \(String(describing: error), privacy: .public)"
                )
            }
        }
        newBrowser.start(queue: queue)
    }

    private func updateDiscoveredServices(_ results: Set<NWBrowser.Result>) {
        guard started else { return }
        var current: [String: NWEndpoint] = [:]
        for result in results {
            guard case let .service(name, _, _, _) = result.endpoint else { continue }
            guard shouldInitiateLanConnection(localToken: instanceTokenString, remoteToken: name) else {
                continue
            }
            let key = serviceKey(result.endpoint)
            current[key] = result.endpoint
            if discoveredEndpoints[key] == nil {
                diagnostics.discovered(String(describing: result.endpoint))
                connect(to: result.endpoint, serviceKey: key)
            }
        }
        for removed in bonjourServiceKeys.subtracting(Set(current.keys)) {
            discoveredEndpoints.removeValue(forKey: removed)
        }
        bonjourServiceKeys = Set(current.keys)
        for (key, endpoint) in current {
            discoveredEndpoints[key] = endpoint
        }
    }

    private func connect(to endpoint: NWEndpoint, serviceKey: String) {
        guard started,
              connections.count < Self.maxConnections,
              outboundAddresses[serviceKey] == nil else { return }
        diagnostics.connecting(String(describing: endpoint))
        let connection = NWConnection(to: endpoint, using: lanParameters())
        addConnection(connection, initiator: true, serviceKey: serviceKey)
    }

    private func accept(_ connection: NWConnection) {
        guard started, connections.count < Self.maxConnections else {
            connection.cancel()
            return
        }
        addConnection(connection, initiator: false, serviceKey: nil)
    }

    private func addConnection(
        _ connection: NWConnection,
        initiator: Bool,
        serviceKey: String?
    ) {
        let address = "lan:\(UUID().uuidString.lowercased())"
        do {
            let link = try LanConnection(
                address: address,
                connection: connection,
                initiator: initiator,
                localPrivateKey: identity.agreeSk,
                owner: self,
                serviceKey: serviceKey
            )
            connections[address] = link
            if let serviceKey {
                outboundAddresses[serviceKey] = address
            }
            link.start(on: queue)
        } catch {
            log.warning("Unable to create LAN cryptographic session")
            connection.cancel()
        }
    }

    fileprivate func trustedUserId(for remoteStaticKey: Data) -> Data? {
        trustedPeerForStaticKey(remoteStaticKey)
    }

    fileprivate func connectionAuthenticated(_ link: LanConnection, userId: Data) {
        guard started, connections[link.address] === link else {
            link.close()
            return
        }
        log.info("Authenticated CruiseMesh peer over local Wi-Fi")
        if let serviceKey = link.serviceKey {
            reconnectAttempts[serviceKey] = 0
        }
        onAuthenticated?(link.address, userId)
    }

    fileprivate func connectionReceivedFrame(_ link: LanConnection, frame: Data) {
        guard started, connections[link.address] === link else { return }
        diagnostics.frameReceived()
        onFrame?(link.address, frame)
    }

    fileprivate func connectionClosed(_ link: LanConnection) {
        guard connections[link.address] === link else { return }
        connections.removeValue(forKey: link.address)
        if let serviceKey = link.serviceKey {
            outboundAddresses.removeValue(forKey: serviceKey)
            let attempt = reconnectAttempts[serviceKey, default: 0]
            reconnectAttempts[serviceKey] = min(attempt + 1, Self.reconnectDelays.count - 1)
            let delay = Self.reconnectDelays[min(attempt, Self.reconnectDelays.count - 1)]
            if !link.wasAuthenticated {
                diagnostics.connectionFailed(
                    discoveredEndpoints[serviceKey].map { String(describing: $0) } ?? serviceKey,
                    reason: "Secure connection failed; CruiseMesh will retry"
                )
            }
            queue.asyncAfter(deadline: .now() + delay) { [weak self] in
                guard let self,
                      started,
                      outboundAddresses[serviceKey] == nil,
                      let endpoint = discoveredEndpoints[serviceKey] else { return }
                connect(to: endpoint, serviceKey: serviceKey)
            }
        }
        if link.wasAuthenticated {
            onDisconnected?(link.address)
        }
    }

    private func startNextScanCandidate(generation: UUID) {
        guard started, scanGeneration == generation, !scanCandidates.isEmpty else { return }
        let host = scanCandidates.removeFirst()
        guard let port = NWEndpoint.Port(rawValue: lanDefaultTcpPort()) else { return }
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(host), port: port)
        let connection = NWConnection(to: endpoint, using: lanParameters())
        let id = UUID()
        scanConnections[id] = connection
        var completed = false
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let self, let connection else { return }
            queue.async { [weak self] in
                guard let self, !completed, self.scanGeneration == generation else { return }
                switch state {
                case .ready:
                    completed = true
                    connection.cancel()
                    self.scanConnections.removeValue(forKey: id)
                    self.diagnostics.discovered("\(host):\(lanDefaultTcpPort())")
                    let key = "scan:\(host):\(lanDefaultTcpPort())"
                    self.discoveredEndpoints[key] = endpoint
                    self.connect(to: endpoint, serviceKey: key)
                    self.diagnostics.scanAdvanced()
                    self.startNextScanCandidate(generation: generation)
                case .failed, .cancelled:
                    completed = true
                    connection.cancel()
                    self.scanConnections.removeValue(forKey: id)
                    self.diagnostics.scanAdvanced()
                    self.startNextScanCandidate(generation: generation)
                default:
                    break
                }
            }
        }
        connection.start(queue: queue)
        queue.asyncAfter(deadline: .now() + Self.scanTimeout) { [weak self, weak connection] in
            guard let self, let connection, !completed, scanGeneration == generation else { return }
            completed = true
            connection.cancel()
            scanConnections.removeValue(forKey: id)
            diagnostics.scanAdvanced()
            startNextScanCandidate(generation: generation)
        }
    }

    private func lanParameters() -> NWParameters {
        let parameters = NWParameters.tcp
        parameters.requiredInterfaceType = .wifi
        parameters.includePeerToPeer = false
        return parameters
    }

    private static let maxConnections = 8
    private static let reconnectDelays: [DispatchTimeInterval] = [
        .seconds(2), .seconds(5), .seconds(15), .seconds(30), .seconds(60), .seconds(300),
    ]
    private static let scanConcurrency = 8
    private static let scanTimeout: DispatchTimeInterval = .milliseconds(350)
}

private final class LanConnection {
    enum Phase {
        case awaitMessage1
        case awaitMessage2
        case awaitMessage3
        case transport
    }

    let address: String
    let serviceKey: String?
    private(set) var wasAuthenticated = false

    private weak var owner: LanTransport?
    private let connection: NWConnection
    private let initiator: Bool
    private let noise: LanNoiseSession
    private var phase: Phase
    private var receiveBuffer = Data()
    private var closed = false
    private var setupTimeout: DispatchWorkItem?

    init(
        address: String,
        connection: NWConnection,
        initiator: Bool,
        localPrivateKey: Data,
        owner: LanTransport,
        serviceKey: String?
    ) throws {
        self.address = address
        self.connection = connection
        self.initiator = initiator
        self.owner = owner
        self.serviceKey = serviceKey
        noise = try LanNoiseSession(initiator: initiator, localPrivateKey: localPrivateKey)
        phase = initiator ? .awaitMessage2 : .awaitMessage1
    }

    func start(on queue: DispatchQueue) {
        let timeout = DispatchWorkItem { [weak self] in
            guard let self, !wasAuthenticated else { return }
            close()
        }
        setupTimeout = timeout
        queue.asyncAfter(deadline: .now() + .seconds(5), execute: timeout)
        connection.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                if initiator {
                    do {
                        try sendPacket(noise.writeHandshakeMessage())
                    } catch {
                        close()
                        return
                    }
                }
                receiveNext()
            case .failed, .cancelled:
                close()
            default:
                break
            }
        }
        connection.start(queue: queue)
    }

    func sendFrame(_ frame: Data) {
        guard wasAuthenticated, !closed else { return }
        do {
            for record in try noise.encryptFrame(frame: frame) {
                try sendPacket(record)
            }
        } catch {
            close()
        }
    }

    func close(notifyOwner: Bool = true) {
        guard !closed else { return }
        closed = true
        setupTimeout?.cancel()
        setupTimeout = nil
        connection.stateUpdateHandler = nil
        connection.cancel()
        if notifyOwner {
            owner?.connectionClosed(self)
        }
    }

    private func receiveNext() {
        guard !closed else { return }
        connection.receive(
            minimumIncompleteLength: 1,
            maximumLength: 64 * 1024
        ) { [weak self] content, _, isComplete, error in
            guard let self, !closed else { return }
            if let content {
                receiveBuffer.append(content)
                do {
                    try drainPackets()
                } catch {
                    close()
                    return
                }
            }
            if isComplete || error != nil {
                close()
            } else {
                receiveNext()
            }
        }
    }

    private func drainPackets() throws {
        while receiveBuffer.count >= 4 {
            let packetSize = receiveBuffer.prefix(4).reduce(UInt32(0)) {
                ($0 << 8) | UInt32($1)
            }
            guard packetSize > 0, packetSize <= UInt32(LanWire.maxPacketSize) else {
                throw LanTransportError.invalidPacketLength
            }
            let end = 4 + Int(packetSize)
            guard receiveBuffer.count >= end else { return }
            let packet = receiveBuffer.subdata(in: 4..<end)
            receiveBuffer.removeSubrange(0..<end)
            try receivePacket(packet)
        }
    }

    private func receivePacket(_ packet: Data) throws {
        switch phase {
        case .awaitMessage1:
            try noise.readHandshakeMessage(message: packet)
            try sendPacket(noise.writeHandshakeMessage())
            phase = .awaitMessage3
        case .awaitMessage2:
            try noise.readHandshakeMessage(message: packet)
            guard let remoteStatic = noise.remoteStaticKey(),
                  let userId = owner?.trustedUserId(for: remoteStatic) else {
                throw LanTransportError.untrustedPeer
            }
            try sendPacket(noise.writeHandshakeMessage())
            try authenticate(userId: userId)
        case .awaitMessage3:
            try noise.readHandshakeMessage(message: packet)
            guard let remoteStatic = noise.remoteStaticKey(),
                  let userId = owner?.trustedUserId(for: remoteStatic) else {
                throw LanTransportError.untrustedPeer
            }
            try authenticate(userId: userId)
        case .transport:
            if let frame = try noise.decryptRecord(record: packet) {
                owner?.connectionReceivedFrame(self, frame: frame)
            }
        }
    }

    private func authenticate(userId: Data) throws {
        guard noise.isHandshakeFinished() else {
            throw LanTransportError.incompleteHandshake
        }
        phase = .transport
        wasAuthenticated = true
        setupTimeout?.cancel()
        setupTimeout = nil
        owner?.connectionAuthenticated(self, userId: userId)
    }

    private func sendPacket(_ packet: Data) throws {
        guard !closed, !packet.isEmpty, packet.count <= LanWire.maxPacketSize else {
            throw LanTransportError.invalidPacketLength
        }
        let size = UInt32(packet.count)
        var framed = Data([
            UInt8((size >> 24) & 0xff),
            UInt8((size >> 16) & 0xff),
            UInt8((size >> 8) & 0xff),
            UInt8(size & 0xff),
        ])
        framed.append(packet)
        connection.send(content: framed, completion: .contentProcessed { [weak self] error in
            if error != nil {
                self?.close()
            }
        })
    }
}

private enum LanWire {
    static let maxPacketSize = 65_535
}

private enum LanTransportError: Error {
    case incompleteHandshake
    case invalidPacketLength
    case untrustedPeer
}

func trustedLanPeerUserId(contacts: [Contact], remoteStaticKey: Data) -> Data? {
    contacts.first(where: { $0.agreePk == remoteStaticKey })?.userId
}

func appleLanServiceType() -> String {
    lanServiceType().trimmingCharacters(in: CharacterSet(charactersIn: "."))
}

func shouldInitiateLanConnection(localToken: String, remoteToken: String) -> Bool {
    localToken != remoteToken && localToken < remoteToken
}

private func serviceKey(_ endpoint: NWEndpoint) -> String {
    String(describing: endpoint)
}

private func isAddressInUse(_ error: Error) -> Bool {
    guard let networkError = error as? NWError,
          case let .posix(code) = networkError else { return false }
    return code == .EADDRINUSE
}
