import Combine
import Foundation

struct LanTransportSnapshot {
    var state = "Waiting for Wi-Fi"
    var localEndpoint: String?
    var lastPeerEndpoint: String?
    var activePeerNames: [String] = []
    var lastError: String?
    var lastProbeLatencyMs: Int64?
    var probeStatus: String?
    var sentFrames: Int64 = 0
    var receivedFrames: Int64 = 0
    var lastActivityAtMs: Int64?
    var scanProgress: Int?
    var scanTotal: Int?
}

final class LanTransportDiagnostics: ObservableObject {
    static let shared = LanTransportDiagnostics()

    @Published private(set) var snapshot = LanTransportSnapshot()

    private let lock = NSLock()
    private var manualConnector: ((LanManualEndpoint) -> Void)?
    private var pendingManualEndpoint: LanManualEndpoint?
    private var probeRequester: (() -> String?)?
    private var scanRequester: (() -> String?)?
    private var activePeers: [String: String] = [:]

    private init() {}

    func requestManualConnection(_ text: String) -> String? {
        guard let endpoint = parseLanManualEndpoint(text) else {
            return "Enter an IP address or host, optionally followed by :port"
        }
        lock.lock()
        let connector = manualConnector
        lock.unlock()
        guard let connector else { return "Start the mesh before connecting over local Wi-Fi" }
        connector(endpoint)
        return nil
    }

    func requestConnectionTest() -> String? {
        lock.lock()
        let requester = probeRequester
        lock.unlock()
        return requester?() ?? "Start the mesh before testing local Wi-Fi"
    }

    func requestSubnetScan() -> String? {
        lock.lock()
        let requester = scanRequester
        lock.unlock()
        return requester?() ?? "Start the mesh before searching the local subnet"
    }

    func register(
        manualConnector: @escaping (LanManualEndpoint) -> Void,
        probeRequester: @escaping () -> String?,
        scanRequester: @escaping () -> String?
    ) {
        lock.lock()
        self.manualConnector = manualConnector
        self.probeRequester = probeRequester
        self.scanRequester = scanRequester
        let pending = pendingManualEndpoint
        pendingManualEndpoint = nil
        lock.unlock()
        if let pending { manualConnector(pending) }
    }

    func unregister() {
        lock.lock()
        manualConnector = nil
        probeRequester = nil
        scanRequester = nil
        lock.unlock()
    }

    func queueManualConnection(_ endpoint: LanManualEndpoint) {
        lock.lock()
        let connector = manualConnector
        if connector == nil { pendingManualEndpoint = endpoint }
        lock.unlock()
        connector?(endpoint)
    }

    func waitingForWifi() {
        lock.lock()
        activePeers.removeAll()
        lock.unlock()
        publish { _ in LanTransportSnapshot() }
    }

    func listening(localEndpoint: String?) {
        publish {
            var next = $0
            next.state = "Listening for CruiseMesh friends"
            next.localEndpoint = localEndpoint
            next.lastError = nil
            return next
        }
    }

    func discovered(_ endpoint: String) {
        publish {
            var next = $0
            next.state = "Found a CruiseMesh device"
            next.lastPeerEndpoint = endpoint
            next.lastError = nil
            return next
        }
    }

    func connecting(_ endpoint: String) {
        publish {
            var next = $0
            next.state = "Connecting securely"
            next.lastPeerEndpoint = endpoint
            next.lastError = nil
            return next
        }
    }

    func connectionFailed(_ endpoint: String, reason: String) {
        publish {
            var next = $0
            next.state = next.activePeerNames.isEmpty ? "Local Wi-Fi is ready" : "Secure local Wi-Fi link active"
            next.lastPeerEndpoint = endpoint
            next.lastError = reason
            return next
        }
    }

    func authenticated(address: String, peerName: String) {
        lock.lock()
        activePeers[address] = peerName
        let names = Array(Set(activePeers.values)).sorted()
        lock.unlock()
        publish {
            var next = $0
            next.state = "Secure local Wi-Fi link active"
            next.activePeerNames = names
            next.lastError = nil
            return next
        }
    }

    func disconnected(address: String) {
        lock.lock()
        activePeers.removeValue(forKey: address)
        let names = Array(Set(activePeers.values)).sorted()
        lock.unlock()
        publish {
            var next = $0
            next.state = names.isEmpty ? "Listening for CruiseMesh friends" : "Secure local Wi-Fi link active"
            next.activePeerNames = names
            return next
        }
    }

    func probeStarted() {
        publish {
            var next = $0
            next.probeStatus = "Testing encrypted LAN link…"
            next.lastError = nil
            return next
        }
    }

    func probeSucceeded(latencyMs: Int64) {
        publish {
            var next = $0
            next.probeStatus = "Encrypted round trip: \(latencyMs) ms"
            next.lastProbeLatencyMs = latencyMs
            next.lastError = nil
            next.lastActivityAtMs = Self.nowMs
            return next
        }
    }

    func probeFailed(_ reason: String) {
        publish {
            var next = $0
            next.probeStatus = nil
            next.lastError = reason
            return next
        }
    }

    func frameSent() {
        publish {
            var next = $0
            next.sentFrames += 1
            next.lastActivityAtMs = Self.nowMs
            return next
        }
    }

    func frameReceived() {
        publish {
            var next = $0
            next.receivedFrames += 1
            next.lastActivityAtMs = Self.nowMs
            return next
        }
    }

    func scanStarted(total: Int) {
        publish {
            var next = $0
            next.scanProgress = 0
            next.scanTotal = total
            next.lastError = nil
            return next
        }
    }

    func scanAdvanced() {
        publish {
            guard let total = $0.scanTotal else { return $0 }
            var next = $0
            let progress = min((next.scanProgress ?? 0) + 1, total)
            next.scanProgress = progress == total ? nil : progress
            next.scanTotal = progress == total ? nil : total
            if progress == total { next.probeStatus = "Local subnet search finished" }
            return next
        }
    }

    func scanCancelled() {
        publish {
            var next = $0
            next.scanProgress = nil
            next.scanTotal = nil
            return next
        }
    }

    private func publish(_ transform: @escaping (LanTransportSnapshot) -> LanTransportSnapshot) {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            snapshot = transform(snapshot)
        }
    }

    private static var nowMs: Int64 {
        Int64(Date().timeIntervalSince1970 * 1_000)
    }
}
