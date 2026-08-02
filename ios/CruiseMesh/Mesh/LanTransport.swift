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
    private let scanPlanner = LanScanPlanner()
    private let instanceToken: Data
    private let instanceTokenString: String

    private var started = false
    private var foregroundActive = true
    private var listener: NWListener?
    private var browser: NWBrowser?
    private var wifiPathMonitor: NWPathMonitor?
    private var activeNetwork: LocalWifiIPv4Network?
    private var announcedEndpoint: LanManualEndpoint?
    private var announcedNetworkId: String?
    private var connections: [String: LanConnection] = [:]
    private var discoveredEndpoints: [String: NWEndpoint] = [:]
    /// Endpoint/service keys peer evidence has already been counted for on
    /// this network join -- repeated evidence about the same key (an
    /// already-connected/linked peer's Bonjour record refreshing, or a
    /// resent endpoint hint) must not keep resetting the full-sweep
    /// backoff. Reset alongside `discoveredEndpoints` on network change, and
    /// bounded (oldest forgotten first) because the key comes from whatever
    /// is advertising -- see `BoundedLanKeySet`.
    private var peerEvidenceSeenKeys = BoundedLanKeySet(limit: LanTransport.maxTrackedPeerKeys)
    /// Keys an election-loser fallback connect has already been scheduled
    /// for on this network join, so repeated browse updates for the same
    /// service don't stack duplicate fallback timers. Reset alongside
    /// `discoveredEndpoints` on network change, and bounded the same way --
    /// refusing new keys at a cap would cost real peers the asymmetric-mDNS
    /// rescue this fallback exists to provide.
    private var electionFallbackKeys = BoundedLanKeySet(limit: LanTransport.maxTrackedPeerKeys)
    /// Accepted contacts that have demonstrated LAN support, as UserID to the
    /// millisecond that support was last seen, pushed in by MeshController
    /// (`updateLanCapableContacts`). The automatic sweep keeps looking while
    /// any of them lacks an authenticated LAN link -- but only while that
    /// evidence is recent (`lanCapabilityMotivatesScan`), so a contact who is
    /// ashore does not keep this phone sweeping the subnet forever.
    private var lanCapableContacts: [Data: Int64] = [:]
    private var bonjourServiceKeys = Set<String>()
    private var outboundAddresses: [String: String] = [:]
    private var reconnectAttempts: [String: Int] = [:]
    private var runningScan: RunningScan?
    private var scanConnections: [UUID: NWConnection] = [:]
    private var automaticScanWorkItem: DispatchWorkItem?
    /// Consecutive completed sweeps whose verdict was `.isolationSuspected`.
    /// A single congested sweep can look isolated (every probe timing out),
    /// so the planner is only told to back off to its cap once the verdict
    /// repeats. Reset on a network change and by any other verdict.
    private var consecutiveIsolationVerdicts = 0

    // FI7: track how long the browser/listener has been stuck `.waiting`
    // so a denied Local Network permission (which iOS reports only as a
    // silent, indefinite `.waiting`, never a distinct error state) can be
    // surfaced instead of retrying forever with no user-visible signal.
    private var browserWaitingSinceMs: Int64?
    private var listenerWaitingSinceMs: Int64?
    private var permissionWarningActive = false

    init(identity: Identity, trustedPeerForStaticKey: @escaping TrustedPeerLookup) {
        self.identity = identity
        self.trustedPeerForStaticKey = trustedPeerForStaticKey
        var uuid = UUID().uuid
        let token = withUnsafeBytes(of: &uuid) { Data($0.prefix(8)) }
        instanceToken = token
        instanceTokenString = token.map { String(format: "%02x", $0) }.joined()
    }

    func start(foregroundActive: Bool = true) {
        queue.async { [weak self] in
            guard let self, !started else { return }
            started = true
            self.foregroundActive = foregroundActive
            startListener(preferDefaultPort: true)
            startBrowser()
            startWifiPathMonitor()
        }
    }

    /// MeshController pushes the contacts that have demonstrated LAN support,
    /// each with the millisecond that support was last seen, whenever the set
    /// changes; the automatic-scan gate compares it against currently
    /// authenticated links and against the recency window.
    func updateLanCapableContacts(_ contacts: [Data: Int64]) {
        queue.async { [weak self] in
            self?.lanCapableContacts = contacts
        }
    }

    func setForegroundActive(_ active: Bool) {
        queue.async { [weak self] in
            guard let self else { return }
            foregroundActive = active
            if active {
                scheduleAutomaticScan(after: .milliseconds(0))
            } else {
                automaticScanWorkItem?.cancel()
                automaticScanWorkItem = nil
                cancelRunningScan()
            }
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
            wifiPathMonitor?.cancel()
            wifiPathMonitor = nil
            automaticScanWorkItem?.cancel()
            automaticScanWorkItem = nil
            cancelRunningScan(updateDiagnostics: false)
            scanPlanner.onNetworkLost()
            consecutiveIsolationVerdicts = 0
            activeNetwork = nil
            announcedEndpoint = nil
            announcedNetworkId = nil
            discoveredEndpoints.removeAll()
            peerEvidenceSeenKeys.removeAll()
            electionFallbackKeys.removeAll()
            bonjourServiceKeys.removeAll()
            outboundAddresses.removeAll()
            reconnectAttempts.removeAll()
            browserWaitingSinceMs = nil
            listenerWaitingSinceMs = nil
            permissionWarningActive = false
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
            // A hinted address came from the contact, not from anything this
            // phone observed, so it gets its own single-shot key.
            let key = remoteInstanceToken == nil
                ? "endpoint:\(endpoint.display)"
                : lanHintConnectKey(endpoint.display)
            let networkEndpoint = NWEndpoint.hostPort(
                host: NWEndpoint.Host(endpoint.host),
                port: NWEndpoint.Port(rawValue: endpoint.port) ?? .any
            )
            if let remoteInstanceToken {
                notePeerEvidence(key: key)
                if !shouldInitiateLanConnection(
                    localToken: instanceTokenString,
                    remoteToken: remoteInstanceToken.map { String(format: "%02x", $0) }.joined()
                ) {
                    log.info(
                        "Resolved LAN peer \(endpoint.display, privacy: .public); awaiting their connection (tie-break)"
                    )
                    scheduleElectionFallback(key: key, endpoint: networkEndpoint)
                    return
                }
            }
            // Only a key this phone found itself is remembered for retry;
            // without a remembered endpoint the retry after a failed hint
            // dial finds nothing and stops there.
            if !isSingleShotLanConnectKey(key) {
                discoveredEndpoints[key] = networkEndpoint
            }
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
        guard let network = localWifiIPv4Network() else {
            return "Connect this phone to Wi-Fi before searching the local subnet"
        }
        return queue.sync {
            guard started else { return "Start the mesh before searching the local subnet" }
            guard foregroundActive else { return "Return to CruiseMesh before searching the local subnet" }
            guard runningScan == nil else { return "A local subnet search is already running" }
            return startSubnetScan(.fullSubnet, network: network, automatic: false)
        }
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
            listenerWaitingSinceMs = nil
            clearPermissionWarningIfNeeded()
            if let port = failedListener.port {
                log.info("Listening for CruiseMesh LAN peers on TCP \(port.rawValue)")
                if let network = localWifiIPv4Network() {
                    networkBecameAvailable(network)
                } else {
                    diagnostics.listening(localEndpoint: nil)
                }
            }
        case .failed(let error):
            failedListener.cancel()
            if listener === failedListener {
                listener = nil
            }
            listenerWaitingSinceMs = nil
            if started, usedDefaultPort, isAddressInUse(error) {
                log.warning("TCP \(lanDefaultTcpPort()) is occupied; using an advertised fallback port")
                startListener(preferDefaultPort: false)
            } else {
                log.warning("LAN listener failed: \(String(describing: error), privacy: .public)")
            }
        case .waiting(let error):
            log.debug(
                "LAN listener waiting: \(String(describing: error), privacy: .public)\(Self.permissionErrorLogSuffix(error))"
            )
            noteWaiting(.listener, error: error)
        default:
            break
        }
    }

    private func startBrowser() {
        let newBrowser = NWBrowser(
            for: .bonjourWithTXTRecord(type: appleLanServiceType(), domain: nil),
            using: lanParameters()
        )
        browser = newBrowser
        newBrowser.browseResultsChangedHandler = { [weak self] results, _ in
            self?.queue.async {
                self?.updateDiscoveredServices(results)
            }
        }
        newBrowser.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                browserWaitingSinceMs = nil
                clearPermissionWarningIfNeeded()
            case .failed(let error):
                browserWaitingSinceMs = nil
                log.warning("LAN discovery failed: \(String(describing: error), privacy: .public)")
                diagnostics.connectionFailed(
                    "Bonjour",
                    reason: "Local Wi-Fi discovery is unavailable; check Local Network permission"
                )
            case .waiting(let error):
                log.debug(
                    "LAN discovery waiting: \(String(describing: error), privacy: .public)\(Self.permissionErrorLogSuffix(error))"
                )
                noteWaiting(.browser, error: error)
            default:
                break
            }
        }
        newBrowser.start(queue: queue)
    }

    private func startWifiPathMonitor() {
        let monitor = NWPathMonitor(requiredInterfaceType: .wifi)
        wifiPathMonitor = monitor
        monitor.pathUpdateHandler = { [weak self] path in
            guard let self else { return }
            queue.async { [weak self] in
                guard let self, started else { return }
                if path.status == .satisfied, let network = localWifiIPv4Network() {
                    networkBecameAvailable(network)
                } else {
                    networkBecameUnavailable()
                }
            }
        }
        monitor.start(queue: queue)
    }

    private func networkBecameAvailable(_ network: LocalWifiIPv4Network) {
        let changed = activeNetwork != network
        if changed {
            if activeNetwork != nil {
                tearDownNetworkLinks()
            }
            cancelRunningScan()
            activeNetwork = network
            announcedEndpoint = nil
            announcedNetworkId = nil
            consecutiveIsolationVerdicts = 0
            scanPlanner.onNetworkJoined(nowMs: Self.nowMs)
            diagnostics.networkJoined()
            scheduleAutomaticScan(after: Self.initialAutomaticScanDelay)
        }
        guard let port = listener?.port else { return }
        let endpoint = LanManualEndpoint(host: network.address, port: port.rawValue)
        let networkId = lanNetworkId(ipv4Address: network.address)
        diagnostics.listening(localEndpoint: endpoint.display)
        if endpoint != announcedEndpoint || networkId != announcedNetworkId {
            announcedEndpoint = endpoint
            announcedNetworkId = networkId
            log.info("LAN session ready on \(endpoint.display, privacy: .public)")
            onNetworkReady?(endpoint, instanceToken, networkId)
        }
    }

    private func networkBecameUnavailable() {
        guard activeNetwork != nil else { return }
        activeNetwork = nil
        announcedEndpoint = nil
        announcedNetworkId = nil
        consecutiveIsolationVerdicts = 0
        scanPlanner.onNetworkLost()
        automaticScanWorkItem?.cancel()
        automaticScanWorkItem = nil
        // `diagnostics.waitingForWifi()` below resets the published snapshot
        // (including the permission-denied flag) to its default; keep this
        // instance's own bookkeeping in sync so a still-stuck browser after
        // Wi-Fi returns is re-evaluated instead of being silently skipped
        // (FI7's flag would otherwise think it already warned).
        browserWaitingSinceMs = nil
        listenerWaitingSinceMs = nil
        permissionWarningActive = false
        cancelRunningScan(updateDiagnostics: false)
        tearDownNetworkLinks()
        diagnostics.waitingForWifi()
    }

    private func tearDownNetworkLinks() {
        discoveredEndpoints.removeAll()
        peerEvidenceSeenKeys.removeAll()
        electionFallbackKeys.removeAll()
        bonjourServiceKeys.removeAll()
        outboundAddresses.removeAll()
        reconnectAttempts.removeAll()
        let active = Array(connections.values)
        connections.removeAll()
        for link in active {
            link.close(notifyOwner: false)
            if link.wasAuthenticated {
                onDisconnected?(link.address)
            }
        }
    }

    private func updateDiscoveredServices(_ results: Set<NWBrowser.Result>) {
        guard started else { return }
        var current: [String: NWEndpoint] = [:]
        for result in results {
            guard case let .service(_, _, _, _) = result.endpoint,
                  case let .bonjour(txtRecord) = result.metadata,
                  let remoteToken = lanBonjourPeerToken(txtRecord.dictionary),
                  remoteToken != instanceTokenString else { continue }
            let key = serviceKey(result.endpoint)
            notePeerEvidence(key: key)
            guard shouldInitiateLanConnection(
                localToken: instanceTokenString,
                remoteToken: remoteToken
            ) else {
                log.info(
                    "Resolved LAN peer \(String(describing: result.endpoint), privacy: .public); awaiting their connection (tie-break)"
                )
                scheduleElectionFallback(key: key, endpoint: result.endpoint)
                continue
            }
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

    /// The tie-break said the peer initiates -- but discovery is often
    /// asymmetric (the peer may never have resolved us, or its connect may
    /// fail), which used to strand both sides forever. If nothing has
    /// connected for this key within `electionFallbackDelay`, initiate
    /// anyway: duplicate connections are safe by design (msg_id
    /// deduplication), and the duplicate-link guard in the Noise handshake
    /// closes a redundant socket before it becomes a second live link.
    private func scheduleElectionFallback(key: String, endpoint: NWEndpoint) {
        let claim = electionFallbackKeys.claim(key)
        if let evicted = claim.evicted { logForgottenLanKey(evicted) }
        guard claim.isNew else { return }
        queue.asyncAfter(deadline: .now() + Self.electionFallbackDelay) { [weak self] in
            guard let self,
                  started,
                  activeNetwork != nil,
                  outboundAddresses[key] == nil else { return }
            log.debug("Tie-break peer never connected; initiating ourselves")
            if !isSingleShotLanConnectKey(key) {
                discoveredEndpoints[key] = endpoint
            }
            connect(to: endpoint, serviceKey: key)
        }
    }

    /// A peer advertised itself (Bonjour result or an endpoint hint) under
    /// `key`. Only genuinely NEW evidence is worth reacting to: an
    /// already-connected/linked peer's record keeps reappearing (TXT
    /// refreshes, periodic browse updates, resent hints) and must not keep
    /// re-triggering full sweeps.
    ///
    /// The key comes from whatever is advertising, so "new" is not a
    /// trustworthy signal on a busy or hostile network: the remembered set is
    /// bounded (`BoundedLanKeySet`) and `LanScanPlanner.onPeerEvidence` only
    /// rewinds the sweep schedule a bounded number of times per network join.
    /// Past either bound the caller still discovers and dials the peer
    /// normally -- only the schedule pull-forward stops.
    private func notePeerEvidence(key: String) {
        let claim = peerEvidenceSeenKeys.claim(key)
        if let evicted = claim.evicted { logForgottenLanKey(evicted) }
        guard claim.isNew else { return }
        diagnostics.peerEvidence()
        guard scanPlanner.onPeerEvidence(nowMs: Self.nowMs) else { return }
        scheduleAutomaticScan(after: Self.peerEvidenceScanDelay)
    }

    /// A per-network-join key was forgotten to stay inside
    /// `BoundedLanKeySet`'s bound, which only happens when far more distinct
    /// services have appeared on this Wi-Fi than any real fleet produces.
    private func logForgottenLanKey(_ key: String) {
        let shortened = String(key.prefix(8))
        log.info("Forgetting the oldest tracked local Wi-Fi peer to make room (\(shortened, privacy: .public))")
    }

    /// Credits the sweep that dialed `link` with having found a friend on
    /// this LAN. Only an authenticated friend -- or one the sweep discovered
    /// is already linked -- counts; see `scanCandidateCompleted`.
    fileprivate func markSweepFoundFriend(dialedBy link: LanConnection) {
        markSweepFoundFriend(scanGeneration: link.scanGeneration)
    }

    /// As above, for the sweep-dialed paths that stop before a connection
    /// object exists. The generation check keeps a probe whose sweep was
    /// replaced or cancelled from crediting whatever sweep is running now.
    private func markSweepFoundFriend(scanGeneration: UUID?) {
        guard var scan = runningScan,
              lanSweepCreditApplies(
                sweepGeneration: scanGeneration,
                runningSweepGeneration: scan.generation
              ) else { return }
        scan.foundPeer = true
        runningScan = scan
    }

    fileprivate func hasAuthenticatedLink(userId: Data) -> Bool {
        connections.values.contains { $0.wasAuthenticated && $0.authenticatedUserId == userId }
    }

    private func connect(to endpoint: NWEndpoint, serviceKey: String, scanGeneration: UUID? = nil) {
        guard started else { return }
        guard connections.count < Self.maxConnections else {
            // The link table is full, which with a friend on one of those
            // links is the healthiest network there is -- not an empty one.
            if lanSweepProbeFoundFriend(
                keyAlreadyAuthenticated: false,
                linkTableFull: true,
                authenticatedLinks: authenticatedLinkCount
            ) {
                markSweepFoundFriend(scanGeneration: scanGeneration)
            }
            return
        }
        guard let existing = outboundAddresses[serviceKey] else {
            diagnostics.connecting(String(describing: endpoint))
            let connection = NWConnection(to: endpoint, using: lanParameters())
            addConnection(
                connection,
                initiator: true,
                serviceKey: serviceKey,
                scanGeneration: scanGeneration
            )
            return
        }
        // A healthy link holds its service key for its whole life, so an
        // authenticated one here means this probe just re-found a friend an
        // earlier sweep already linked. That is a find -- without crediting
        // it, every sweep after the one that linked the family reports
        // "nobody home" and arms the expensive tier on a working network.
        if lanSweepProbeFoundFriend(
            keyAlreadyAuthenticated: connections[existing]?.wasAuthenticated == true,
            linkTableFull: false,
            authenticatedLinks: authenticatedLinkCount
        ) {
            markSweepFoundFriend(scanGeneration: scanGeneration)
        }
    }

    private var authenticatedLinkCount: Int {
        connections.values.filter(\.wasAuthenticated).count
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
        serviceKey: String?,
        scanGeneration: UUID? = nil
    ) {
        let address = "lan:\(UUID().uuidString.lowercased())"
        do {
            let link = try LanConnection(
                address: address,
                connection: connection,
                initiator: initiator,
                localPrivateKey: identity.agreeSk,
                owner: self,
                serviceKey: serviceKey,
                scanGeneration: scanGeneration
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
        // Only an authenticated friend counts as a sweep find -- a bare TCP
        // connect could be any unrelated service on the default port, and
        // must not disarm the full-subnet tier.
        markSweepFoundFriend(dialedBy: link)
        onAuthenticated?(link.address, userId)
        scheduleAutomaticScan(after: Self.automaticScanRetryInterval)
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
            if link.abortedDuplicateLink {
                // Not a failure: the contact already has a live LAN link.
                // No retry -- rediscovery covers a later drop of the
                // surviving link.
                reconnectAttempts.removeValue(forKey: serviceKey)
            } else if serviceKey.hasPrefix("scan:"), !link.wasAuthenticated {
                // A successful TCP connect can still be an unrelated service
                // on the default port. Do not retain or retry it after Noise
                // rejects the peer; explicit scans remain bounded.
                discoveredEndpoints.removeValue(forKey: serviceKey)
                reconnectAttempts.removeValue(forKey: serviceKey)
                diagnostics.connectionFailed(
                    serviceKey,
                    reason: "The discovered TCP service was not an accepted CruiseMesh friend"
                )
            } else {
                let attempt = reconnectAttempts[serviceKey, default: 0]
                reconnectAttempts[serviceKey] = min(attempt + 1, Self.reconnectDelays.count - 1)
                let delay = Self.reconnectDelays[min(attempt, Self.reconnectDelays.count - 1)]
                if !link.wasAuthenticated {
                    diagnostics.connectionFailed(
                        discoveredEndpoints[serviceKey].map { String(describing: $0) } ?? serviceKey,
                        reason: "Secure connection failed; CruiseMesh will retry"
                    )
                    // A failed secure setup to a discovered/hinted peer:
                    // check promptly whether a fallback sweep is due
                    // instead of waiting out the periodic interval.
                    scheduleAutomaticScan(after: Self.reconnectAutomaticScanDelay)
                }
                queue.asyncAfter(deadline: .now() + delay) { [weak self] in
                    guard let self,
                          started,
                          outboundAddresses[serviceKey] == nil,
                          let endpoint = discoveredEndpoints[serviceKey] else { return }
                    connect(to: endpoint, serviceKey: serviceKey)
                }
            }
        }
        if link.wasAuthenticated {
            onDisconnected?(link.address)
            scheduleAutomaticScan(after: Self.reconnectAutomaticScanDelay)
        }
    }

    private func startSubnetScan(
        _ breadth: LanScanBreadth,
        network: LocalWifiIPv4Network,
        automatic: Bool
    ) -> String? {
        guard runningScan == nil else { return "A local subnet search is already running" }
        guard !automatic || foregroundActive else { return "Automatic scans run only in the foreground" }
        let prefixLength = breadth == .fullSubnet
            ? network.prefixLength
            : max(network.prefixLength, defaultLanScanPrefixLength)
        // The automatic full-subnet sweep is capped at /20 (~4,094 hosts);
        // the user-initiated "Search local subnet" action (automatic ==
        // false) keeps the wider /16 (~65k hosts) ceiling since the user
        // explicitly asked for it.
        let effectivePrefix = automatic
            ? effectiveAutomaticLanScanPrefixLength(prefixLength)
            : effectiveLanScanPrefixLength(prefixLength)
        let candidates = lanSubnetHosts(
            localAddress: network.address,
            prefixLength: effectivePrefix
        ).shuffled()
        guard !candidates.isEmpty else { return "CruiseMesh could not determine the local subnet" }
        let generation = UUID()
        runningScan = RunningScan(
            generation: generation,
            breadth: breadth,
            prefixLength: effectivePrefix,
            candidates: candidates,
            nextCandidateIndex: 0,
            remaining: candidates.count,
            foundPeer: false
        )
        log.info(
            "Scanning \(candidates.count) subnet hosts (/\(effectivePrefix)) for CruiseMesh peers"
        )
        diagnostics.scanStarted(total: candidates.count)
        for _ in 0..<min(Self.scanConcurrency, candidates.count) {
            startNextScanCandidate(generation: generation)
        }
        return nil
    }

    private func startNextScanCandidate(generation: UUID) {
        guard started,
              foregroundActive,
              var scan = runningScan,
              scan.generation == generation,
              scan.nextCandidateIndex < scan.candidates.count else { return }
        let host = scan.candidates[scan.nextCandidateIndex]
        scan.nextCandidateIndex += 1
        runningScan = scan
        guard let port = NWEndpoint.Port(rawValue: lanDefaultTcpPort()) else { return }
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(host), port: port)
        let connection = NWConnection(to: endpoint, using: lanParameters())
        let id = UUID()
        scanConnections[id] = connection
        var completed = false
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let self, let connection else { return }
            queue.async { [weak self] in
                guard let self,
                      !completed,
                      self.runningScan?.generation == generation else { return }
                switch state {
                case .ready:
                    completed = true
                    connection.cancel()
                    self.scanConnections.removeValue(forKey: id)
                    self.diagnostics.discovered("\(host):\(lanDefaultTcpPort())")
                    let key = "scan:\(host):\(lanDefaultTcpPort())"
                    self.discoveredEndpoints[key] = endpoint
                    self.connect(to: endpoint, serviceKey: key, scanGeneration: generation)
                    // A bare TCP connect is not a find -- only the Noise
                    // handshake authenticating a friend marks the sweep
                    // (see connectionAuthenticated). It is still a
                    // `.connected` probe outcome: the network carried it.
                    self.scanCandidateCompleted(generation: generation, outcome: .connected)
                case .failed(let error):
                    completed = true
                    connection.cancel()
                    self.scanConnections.removeValue(forKey: id)
                    self.scanCandidateCompleted(
                        generation: generation,
                        outcome: classifyLanSweepProbeFailure(error)
                    )
                case .cancelled:
                    completed = true
                    connection.cancel()
                    self.scanConnections.removeValue(forKey: id)
                    self.scanCandidateCompleted(generation: generation, outcome: .other)
                default:
                    break
                }
            }
        }
        connection.start(queue: queue)
        queue.asyncAfter(deadline: .now() + Self.scanTimeout) { [weak self, weak connection] in
            guard let self,
                  let connection,
                  !completed,
                  runningScan?.generation == generation else { return }
            completed = true
            connection.cancel()
            scanConnections.removeValue(forKey: id)
            // Our own attempt timeout: the probe went out and nothing came
            // back, which is the signal a client-isolated network produces.
            scanCandidateCompleted(generation: generation, outcome: .timedOut)
        }
    }

    /// One candidate of the running sweep retired. When it was the last one,
    /// the tallied outcomes become the sweep verdict: `foundPeer` (an
    /// authenticated friend, set by `connectionAuthenticated`) still decides
    /// whether the full-subnet tier arms, while the verdict decides what
    /// diagnostics says and whether the planner should defer to its backoff
    /// cap.
    private func scanCandidateCompleted(generation: UUID, outcome: LanSweepProbeOutcome) {
        guard var scan = runningScan, scan.generation == generation else { return }
        scan.remaining = max(scan.remaining - 1, 0)
        scan.outcomes.record(outcome)
        diagnostics.scanAdvanced()
        if scan.remaining == 0 {
            runningScan = nil
            scanPlanner.onScanCompleted(scan.breadth, nowMs: Self.nowMs, foundPeer: scan.foundPeer)
            log.info("\(scan.outcomes.logLine(prefixLength: scan.prefixLength), privacy: .public)")
            diagnostics.sweepCompleted(scan.outcomes)
            if lanSweepVerdict(scan.outcomes) == .isolationSuspected {
                // One congested sweep can time out every probe and look
                // isolated; only a repeat verdict jumps the planner to its
                // backoff cap.
                consecutiveIsolationVerdicts += 1
                if consecutiveIsolationVerdicts >= Self.isolationConfirmSweeps {
                    scanPlanner.onIsolationSuspected(nowMs: Self.nowMs)
                }
            } else {
                consecutiveIsolationVerdicts = 0
            }
            if scan.breadth == .local24 {
                scheduleAutomaticScan(after: Self.escalateAutomaticScanDelay)
            }
        } else {
            runningScan = scan
            startNextScanCandidate(generation: generation)
        }
    }

    private func cancelRunningScan(updateDiagnostics: Bool = true) {
        guard runningScan != nil || !scanConnections.isEmpty else { return }
        runningScan = nil
        scanConnections.values.forEach { $0.cancel() }
        scanConnections.removeAll()
        if updateDiagnostics {
            diagnostics.scanCancelled()
        }
    }

    private func scheduleAutomaticScan(after delay: DispatchTimeInterval) {
        automaticScanWorkItem?.cancel()
        guard started, activeNetwork != nil, foregroundActive else { return }
        let work = DispatchWorkItem { [weak self] in
            self?.runAutomaticScanCheck()
        }
        automaticScanWorkItem = work
        queue.asyncAfter(deadline: .now() + delay, execute: work)
    }

    private func runAutomaticScanCheck() {
        automaticScanWorkItem = nil
        guard started, foregroundActive, let network = activeNetwork else { return }
        let pendingOutbound = outboundAddresses.values
            .filter { connections[$0]?.wasAuthenticated != true }
            .count
        let linked = Set(connections.values.compactMap {
            $0.wasAuthenticated ? $0.authenticatedUserId : nil
        })
        let nowMs = Self.nowMs
        let motivating = lanCapableContacts.filter { userId, lastSupportedAtMs in
            !linked.contains(userId)
                && lanCapabilityMotivatesScan(lastSupportedAtMs: lastSupportedAtMs, nowMs: nowMs)
        }.count
        if shouldRunAutomaticLanScan(
            activeConnections: connections.count,
            pendingOutboundAttempts: pendingOutbound,
            scanRemaining: runningScan?.remaining ?? 0,
            unlinkedCapableContacts: motivating
        ), let breadth = scanPlanner.takeDueScan(nowMs: nowMs) {
            log.info("Starting automatic local Wi-Fi fallback search (\(String(describing: breadth)))")
            _ = startSubnetScan(breadth, network: network, automatic: true)
        }
        scheduleAutomaticScan(after: Self.automaticScanRetryInterval)
    }

    // MARK: - FI7: Local Network permission surfacing
    //
    // iOS never reports a denied Local Network permission as a distinct
    // error the way it does Bluetooth authorization -- browsing/listening
    // just sits in `.waiting` forever, optionally carrying a POSIX/DNS-SD
    // error that isn't guaranteed to be present or stable. So the signal
    // used here is behavioral: `.waiting` that persists for a while *while
    // Wi-Fi itself is reachable* (ruling out the ordinary "no Wi-Fi yet"
    // case, which the path monitor already reports separately via
    // `waitingForWifi()`). The known error codes below are logged as a
    // corroborating detail when present, but never gate the decision --
    // this could not be exercised against a real denial on this build
    // machine, so the detection is intentionally defensive.

    private enum LanWaitTarget {
        case browser
        case listener
    }

    /// How long `.waiting` has to persist (with Wi-Fi reachable) before it
    /// looks like a denied permission rather than an ordinary transient
    /// wait (e.g. right after the interface comes up).
    private static let permissionWarningThresholdMs: Int64 = 4_000

    private func noteWaiting(_ target: LanWaitTarget, error: NWError) {
        let now = Self.nowMs
        switch target {
        case .browser:
            if browserWaitingSinceMs == nil { browserWaitingSinceMs = now }
        case .listener:
            if listenerWaitingSinceMs == nil { listenerWaitingSinceMs = now }
        }
        queue.asyncAfter(deadline: .now() + .milliseconds(Int(Self.permissionWarningThresholdMs))) { [weak self] in
            self?.evaluatePermissionWarning()
        }
    }

    private func evaluatePermissionWarning() {
        guard started else { return }
        let now = Self.nowMs
        // Only meaningful once Wi-Fi is actually reachable -- otherwise this
        // is the ordinary "no Wi-Fi" case, surfaced separately.
        let wifiReachable = activeNetwork != nil || localWifiIPv4Network() != nil
        let browserStuck = shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: browserWaitingSinceMs,
            nowMs: now,
            thresholdMs: Self.permissionWarningThresholdMs,
            wifiReachable: wifiReachable
        )
        let listenerStuck = shouldWarnAboutLocalNetworkPermission(
            waitingSinceMs: listenerWaitingSinceMs,
            nowMs: now,
            thresholdMs: Self.permissionWarningThresholdMs,
            wifiReachable: wifiReachable
        )
        guard browserStuck || listenerStuck else { return }
        guard !permissionWarningActive else { return }
        permissionWarningActive = true
        log.warning(
            "Local Network permission looks denied: LAN discovery/listening has been waiting \(Self.permissionWarningThresholdMs) ms while Wi-Fi is reachable"
        )
        diagnostics.localNetworkPermissionLikelyDenied()
    }

    private func clearPermissionWarningIfNeeded() {
        guard permissionWarningActive else { return }
        permissionWarningActive = false
        diagnostics.localNetworkPermissionResolved()
    }

    /// Best-effort corroborating detail for the debug log only -- does not
    /// gate `evaluatePermissionWarning()`. See the MARK above.
    private static func permissionErrorLogSuffix(_ error: NWError) -> String {
        isKnownLocalNetworkPermissionError(error)
            ? " (matches a documented Local Network permission-denied error)"
            : ""
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
    private static let scanConcurrency = 64
    private static let scanTimeout: DispatchTimeInterval = .milliseconds(350)
    private static let initialAutomaticScanDelay: DispatchTimeInterval = .seconds(5)
    private static let reconnectAutomaticScanDelay: DispatchTimeInterval = .seconds(2)
    // A prompt recheck after a /24 sweep completes, not an escalation
    // trigger by itself: `LanScanPlanner` only arms the full-subnet tier on
    // an empty /24 sweep and holds it off for
    // `LanScanPlanner.emptyLocalSweepFullDelayMs` (60s) after that, so this
    // recheck will usually find nothing due yet.
    private static let escalateAutomaticScanDelay: DispatchTimeInterval = .seconds(2)
    private static let automaticScanRetryInterval: DispatchTimeInterval = .seconds(5 * 60)
    /// How long the tie-break loser waits for the elected side's connection
    /// before initiating anyway -- covers the winner's worst case (connect
    /// plus handshake timeouts) with margin.
    private static let electionFallbackDelay: DispatchTimeInterval = .seconds(15)
    /// Prompt scan-check pull-forward when fresh peer evidence arrives; the
    /// planner and loneliness gate still decide whether anything runs.
    private static let peerEvidenceScanDelay: DispatchTimeInterval = .seconds(2)
    /// Consecutive `.isolationSuspected` sweep verdicts required before the
    /// planner defers full sweeps to its backoff cap.
    private static let isolationConfirmSweeps = 2
    /// Ceiling on the per-network-join bookkeeping sets (seen peer keys,
    /// scheduled election fallbacks, browse results acted on). Their keys
    /// come from whatever advertises on the Wi-Fi, so they are only as
    /// bounded as the network is honest; 256 is far above any real fleet and
    /// keeps a busy network from growing them without limit.
    static let maxTrackedPeerKeys = 256

    private static var nowMs: Int64 {
        Int64(Date().timeIntervalSince1970 * 1_000)
    }

    private struct RunningScan {
        let generation: UUID
        let breadth: LanScanBreadth
        let prefixLength: Int
        let candidates: [String]
        var nextCandidateIndex: Int
        var remaining: Int
        /// Tally of how each probe finished, which becomes the sweep verdict
        /// once the last candidate retires. Only touched on the transport
        /// queue.
        var outcomes = LanSweepOutcomeSummary()
        /// Whether a "scan:"-keyed connection authenticated an accepted
        /// friend while this sweep ran -- feeds `LanScanPlanner
        /// .onScanCompleted`'s `foundPeer`, which decides whether an empty
        /// /24 sweep arms the full-subnet tier. A bare TCP connect
        /// deliberately does not count: any unrelated service on the
        /// default port must not disarm the wider sweep.
        var foundPeer: Bool
    }
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
    /// The sweep generation that dialed this link, if a subnet sweep did.
    /// Only that sweep may be credited with what this handshake finds -- see
    /// `LanTransport.markSweepFoundFriend`.
    let scanGeneration: UUID?
    private(set) var wasAuthenticated = false
    /// The accepted contact this link authenticated as, for the transport's
    /// duplicate-link and unlinked-capable-contact checks.
    private(set) var authenticatedUserId: Data?
    /// Set when the initiator-side handshake found the contact already
    /// linked over LAN: the close is deliberate, not a failure to retry.
    private(set) var abortedDuplicateLink = false

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
        serviceKey: String?,
        scanGeneration: UUID? = nil
    ) throws {
        self.address = address
        self.connection = connection
        self.initiator = initiator
        self.owner = owner
        self.serviceKey = serviceKey
        self.scanGeneration = scanGeneration
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
            if owner?.hasAuthenticatedLink(userId: userId) == true {
                // Election fallbacks and sweeps may dial a contact that
                // connected to us in the meantime. Close the redundant
                // socket before it becomes a second live link -- but a sweep
                // that ran into a friend it is already linked to has still
                // proved discovery works on this network, so credit it
                // exactly as an authenticated find would. Otherwise every
                // sweep on a healthy network reports "found nobody" and arms
                // the expensive full tier.
                owner?.markSweepFoundFriend(dialedBy: self)
                abortedDuplicateLink = true
                throw LanTransportError.duplicateLink
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
        authenticatedUserId = userId
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
    case duplicateLink
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

func lanBonjourPeerToken(_ txtRecord: [String: String]) -> String? {
    guard txtRecord["v"] == "1",
          let token = txtRecord["i"],
          !token.isEmpty else { return nil }
    return token
}

/// The "have I already handled this?" memory the transport keeps for one
/// network join: seen peer keys and scheduled election fallbacks.
///
/// Every key comes from something a device on the Wi-Fi chose, so the set
/// cannot be bounded by honesty alone -- a network full of made-up service
/// names would grow it without limit. It is therefore capped at `limit`, and
/// at the cap the OLDEST key is forgotten rather than the newest refused.
/// That direction matters: refusing new keys would let a flood of made-up
/// ones permanently lock out a real family member who joins afterwards,
/// silently. Forgetting the oldest only risks repeating work already done
/// once, which every caller here tolerates.
struct BoundedLanKeySet {
    /// Whether the claimed key is brand-new work, and which key (if any) was
    /// forgotten to make room for it.
    struct Claim: Equatable {
        let isNew: Bool
        let evicted: String?
    }

    private let limit: Int
    private var keys = Set<String>()
    private var order: [String] = []

    init(limit: Int) {
        precondition(limit > 0)
        self.limit = limit
    }

    var count: Int { keys.count }

    mutating func claim(_ key: String) -> Claim {
        guard keys.insert(key).inserted else { return Claim(isNew: false, evicted: nil) }
        order.append(key)
        guard keys.count > limit else { return Claim(isNew: true, evicted: nil) }
        let oldest = order.removeFirst()
        keys.remove(oldest)
        return Claim(isNew: true, evicted: oldest)
    }

    func contains(_ key: String) -> Bool { keys.contains(key) }

    mutating func remove(_ key: String) {
        guard keys.remove(key) != nil else { return }
        if let index = order.firstIndex(of: key) { order.remove(at: index) }
    }

    mutating func removeAll() {
        keys.removeAll()
        order.removeAll()
    }
}

/// Whether a sweep probe that stopped before it could open a link still
/// found a friend on this network.
///
/// Both stopping points look like "nothing here" from inside the probe and
/// are anything but: `keyAlreadyAuthenticated` means the address is already
/// carrying an authenticated link to a friend (a healthy link holds its
/// service key for its whole life, so every sweep after the one that linked
/// the family collides here), and a full link table with a friend on it is
/// the healthiest network there is. A full table of in-flight handshakes to
/// unrelated services is not, hence `authenticatedLinks`.
func lanSweepProbeFoundFriend(
    keyAlreadyAuthenticated: Bool,
    linkTableFull: Bool,
    authenticatedLinks: Int
) -> Bool {
    keyAlreadyAuthenticated || (linkTableFull && authenticatedLinks > 0)
}

/// Whether a connection dialed by the sweep at `sweepGeneration` may still
/// credit that sweep with a find. A handshake can finish after its own sweep
/// completed, was cancelled, or was replaced by a newer one; crediting then
/// would either do nothing useful or, worse, mark a sweep that never met the
/// peer. A connection no sweep dialed (`nil`) never credits one.
func lanSweepCreditApplies(sweepGeneration: UUID?, runningSweepGeneration: UUID?) -> Bool {
    guard let sweepGeneration, let runningSweepGeneration else { return false }
    return sweepGeneration == runningSweepGeneration
}

/// Whether a contact that once demonstrated LAN support should still keep the
/// automatic sweep running. Capability itself never expires -- a contact who
/// supports LAN endpoints always will -- but "might be on this Wi-Fi right
/// now" does, and that is what the sweep is spending battery on. Without a
/// bound, one family member who stayed ashore keeps every remaining phone
/// sweeping the subnet forever.
///
/// `lanCapabilityRecencyWindowMs` is deliberately generous: any LAN link, any
/// endpoint hint over Bluetooth, and any hint through the relay all refresh
/// the timestamp, so a contact who is genuinely nearby re-motivates sweeps
/// within seconds of the first contact of a trip. A contact with no such
/// evidence for two weeks is not worth a subnet sweep every five minutes.
func lanCapabilityMotivatesScan(
    lastSupportedAtMs: Int64?,
    nowMs: Int64,
    windowMs: Int64 = lanCapabilityRecencyWindowMs
) -> Bool {
    guard let lastSupportedAtMs else { return false }
    return nowMs - lastSupportedAtMs < windowMs
}

/// Two weeks; see `lanCapabilityMotivatesScan`.
let lanCapabilityRecencyWindowMs: Int64 = 14 * 24 * 60 * 60 * 1_000

/// Whether the periodic check may claim a scan from `LanScanPlanner`. A scan
/// is worthwhile while the transport has no links at all, OR while some
/// contact that has recently demonstrated LAN support still has no
/// authenticated LAN link (`lanCapabilityMotivatesScan`) -- one connected
/// family member must not stop discovery of the rest.
/// In-flight work (pending outbound attempts, a running sweep) always defers.
func shouldRunAutomaticLanScan(
    activeConnections: Int,
    pendingOutboundAttempts: Int,
    scanRemaining: Int,
    unlinkedCapableContacts: Int
) -> Bool {
    (activeConnections == 0 || unlinkedCapableContacts > 0) &&
        pendingOutboundAttempts == 0 &&
        scanRemaining == 0
}

/// FI7: whether an `NWError` matches one of the documented signals for a
/// denied Local Network permission. This is a corroborating detail only
/// (surfaced in the debug log) -- it never gates the actual warning, since
/// neither error is guaranteed to be present or stable across iOS versions
/// (see `shouldWarnAboutLocalNetworkPermission`).
func isKnownLocalNetworkPermissionError(_ error: NWError) -> Bool {
    switch error {
    // dns_sd.h: kDNSServiceErr_PolicyDenied = -65570. Referenced by raw
    // value instead of `import dnssd` so this file doesn't take on a new
    // module dependency for a log-only corroborating signal.
    case .dns(let code):
        return code == -65570
    case .posix(let code):
        return code == .EPERM
    default:
        return false
    }
}

/// FI7: whether a browser/listener stuck `.waiting` since `waitingSinceMs`
/// looks like a denied Local Network permission rather than an ordinary
/// transient wait. iOS reports permission denial only as an indefinite,
/// otherwise-unremarkable `.waiting` -- so the signal here is behavioral
/// (sustained + Wi-Fi otherwise reachable), not a specific error code.
func shouldWarnAboutLocalNetworkPermission(
    waitingSinceMs: Int64?,
    nowMs: Int64,
    thresholdMs: Int64,
    wifiReachable: Bool
) -> Bool {
    guard wifiReachable, let since = waitingSinceMs else { return false }
    return nowMs - since >= thresholdMs
}

private func serviceKey(_ endpoint: NWEndpoint) -> String {
    String(describing: endpoint)
}

private func isAddressInUse(_ error: Error) -> Bool {
    guard let networkError = error as? NWError,
          case let .posix(code) = networkError else { return false }
    return code == .EADDRINUSE
}
