import AVFoundation
import Combine
import Foundation
import Network
import os.log

/// Owns BLE dual-role + frame handling + relay sync (Android `MeshService` parity).
@MainActor
final class MeshController: ObservableObject {
    static let shared = MeshController()

    private let log = Logger(subsystem: "com.cruisemesh", category: "MeshController")
    private let transport = BleTransport()
    private var lanTransport: LanTransport?
    private let lanHealth = LanHealthTracker()
    private let store = AppStore.get()
    private let bluetoothAudioBackoff = BluetoothAudioBackoff()
    private var identity: Identity!
    private var relayTimer: Timer?
    /// CP2b: epoch ms until which relayd asked us not to sync again
    /// (`Retry-After` on a 429); 0 = no backoff. `runRelaySync` drops nudges
    /// inside the window; the 60 s poll tick retries once it has passed.
    private var relayRateLimitedUntilMs: Int64 = 0
    private var pathMonitor: NWPathMonitor?
    /// DTN_TODOS.md D3 (iOS half of audit finding F1, "relay poll-only"): opens
    /// relayd's `GET /ws` push socket (see `RelayPushClient`'s class doc) once
    /// the mesh is running, an identity/relay config exist, and the network
    /// path is satisfied, and calls `runRelaySync()` on every pushed frame
    /// instead of waiting for the next `relayTimer` tick. Never processes
    /// envelope content itself -- see `RelayPushClient`'s class doc. Mirrors
    /// Android's `relayPushClient` / `updateRelayPushSubscription`
    /// (`MeshService.kt`).
    ///
    /// Battery, 2026-07-21: also reports its connection health via
    /// `onRelayPushHealthChanged`, which `reschedulePoll` (through
    /// `RelayPollPolicy.relayPollIntervalMs`) uses to slow `relayTimer` down
    /// to a safety net while push is healthy and the app is foregrounded.
    private lazy var relayPushClient = RelayPushClient(
        onPush: { [weak self] in
            Task { @MainActor in self?.runRelaySync() }
        },
        onHealthChanged: { [weak self] healthy in
            Task { @MainActor in self?.onRelayPushHealthChanged(healthy) }
        }
    )
    private var isRunning = false
    private var meshRolesRunning = false
    private var bluetoothAudioConnected = false
    private var relayCancellable: AnyCancellable?
    /// Health `relayPushClient` reported at the last poll-interval decision;
    /// `nil` before the first one. See `onRelayPushHealthChanged`/
    /// `reschedulePoll`. Mirrors Android's `MeshService.lastKnownPushHealthy`.
    private var lastKnownPushHealthy: Bool?
    private var lanHealthTimer: Timer?
    // D8: periodic re-digest bookkeeping.
    private var digestMaintenanceTimer: Timer?
    private var lastDigestAtByAddress: [String: Int64] = [:]
    private var audioRouteObserver: NSObjectProtocol?
    private var relaySyncInFlight = false
    private var relaySyncPending = false
    private var currentLanEndpoint: LanManualEndpoint?
    private var currentLanInstanceToken: Data?
    private var currentLanNetworkId: String?
    private var appForeground = true

    private init() {}

    func configure(identity: Identity) {
        self.identity = identity
    }

    func start() {
        if isRunning {
            // Repeat start while already running: refresh status only.
            MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
            return
        }
        isRunning = true
        MeshRuntimeStatus.shared.markStarting()

        MeshRouter.registerCentral { [weak self] address, frame in
            self?.transport.sendAsCentral(address: address, frame: frame)
        }
        MeshRouter.registerPeripheral { [weak self] address, frame in
            self?.transport.sendAsPeripheral(address: address, frame: frame)
        }
        let lan = LanTransport(
            identity: identity,
            trustedPeerForStaticKey: { [store = self.store] remoteStaticKey in
                trustedLanPeerUserId(
                    contacts: (try? store.listContacts()) ?? [],
                    remoteStaticKey: remoteStaticKey
                )
            }
        )
        lanTransport = lan
        LanTransportDiagnostics.shared.register(
            manualConnector: { [weak lan] endpoint in
                lan?.connect(endpoint, manual: true)
            },
            probeRequester: { [weak self] in
                self?.requestLanProbe()
            },
            scanRequester: { [weak lan] in
                lan?.startSubnetScan() ?? "Start the mesh before searching the local subnet"
            }
        )
        MeshRouter.registerLan { [weak lan] address, frame in
            lan?.sendFrame(address: address, frame: frame)
        }
        lan.onNetworkReady = { [weak self, weak lan] endpoint, instanceToken, networkId in
            Task { @MainActor in
                guard let self, let lan, self.isRunning else { return }
                self.currentLanEndpoint = endpoint
                self.currentLanInstanceToken = instanceToken
                self.currentLanNetworkId = networkId
                for contact in (try? self.store.listContacts()) ?? [] {
                    if let cached = LanEndpointCache.load(networkId: networkId, userId: contact.userId) {
                        lan.connect(cached)
                    }
                }
                LanEndpointSender.queueToAllCapableContacts(
                    store: self.store,
                    identity: self.identity,
                    endpoint: endpoint,
                    instanceToken: instanceToken,
                    networkId: networkId
                )
                for route in MeshRouter.identifiedRoutes() {
                    self.sendLanEndpointHint(address: route.address)
                }
            }
        }
        lan.onAuthenticated = { [weak self] address, userId in
            Task { @MainActor in
                guard let self, self.isRunning else { return }
                MeshRouter.onConnected(address: address, transport: .lan)
                guard MeshRouter.onHello(address: address, userId: userId) else { return }
                MeshConnectivityStatus.shared.mergeLastSeen(
                    userId: userId,
                    seenAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
                )
                let name = (try? self.store.getContact(userId: userId))?.name
                    ?? String(UserIdHex.encode(userId).prefix(8))
                if (try? self.store.getContact(userId: userId)) != nil {
                    // An authenticated LAN link is the strongest possible
                    // evidence this contact shares a LAN with us, so it also
                    // refreshes the capability recency the automatic-scan
                    // gate reads.
                    LanCapabilityStore.markSupported(userId: userId)
                    self.refreshLanCapableContacts()
                    self.recordPeerConnection(
                        userId: userId,
                        transport: .lan,
                        kind: .connected
                    )
                }
                LanTransportDiagnostics.shared.authenticated(address: address, peerName: name)
                self.sendHello(address: address)
                self.sendLanEndpointHint(address: address)
                self.queueCurrentLanEndpoint(to: userId)
                self.refreshNearby()
            }
        }
        lan.onDisconnected = { [weak self] address in
            Task { @MainActor in
                guard let self, self.isRunning else { return }
                self.recordPeerDisconnected(address: address)
                self.lanHealth.remove(address: address)
                LanTransportDiagnostics.shared.disconnected(address: address)
                MeshRouter.onDisconnected(address: address)
                self.refreshNearby()
            }
        }
        lan.onFrame = { [weak self] address, frame in
            Task { @MainActor in
                guard let self, self.isRunning else { return }
                self.onFrameReceived(address: address, frame: frame)
            }
        }
        lan.start(foregroundActive: appForeground)
        refreshLanCapableContacts()
        startLanHealthLoop()
        startDigestMaintenanceLoop()

        transport.onFrame = { [weak self] address, frame in
            Task { @MainActor in self?.onFrameReceived(address: address, frame: frame) }
        }
        transport.onCentralConnected = { [weak self] address in
            Task { @MainActor in
                MeshRouter.onConnected(address: address, transport: .central)
                self?.sendHello(address: address)
                self?.refreshNearby()
            }
        }
        transport.onCentralDisconnected = { address in
            // Hop via the same Task { @MainActor } pattern as the connect
            // callbacks above (FI6): task-enqueue order preserves the BLE
            // queue's event order, so a fast connect->disconnect can't have
            // its disconnect processed first and re-register a dead route.
            Task { @MainActor in
                MeshController.shared.recordPeerDisconnected(address: address)
                MeshRouter.onDisconnected(address: address)
                MeshController.shared.refreshNearby()
            }
        }
        transport.onPeripheralSubscribed = { [weak self] address in
            Task { @MainActor in
                MeshRouter.onConnected(address: address, transport: .peripheral)
                self?.sendHello(address: address)
                self?.refreshNearby()
            }
        }
        transport.onPeripheralUnsubscribed = { address in
            Task { @MainActor in
                MeshController.shared.recordPeerDisconnected(address: address)
                MeshRouter.onDisconnected(address: address)
                MeshController.shared.refreshNearby()
            }
        }

        registerBluetoothAudioObserver()
        startRelayLoop()
        startMeshRoles()
        refreshBluetoothAudioState(reason: "mesh start")
        MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
        log.info("Mesh started")
    }

    func stop() {
        guard isRunning else { return }
        isRunning = false
        bluetoothAudioConnected = false
        bluetoothAudioBackoff.reset()
        unregisterBluetoothAudioObserver()
        lanTransport?.stop()
        lanTransport = nil
        LanTransportDiagnostics.shared.unregister()
        lanHealthTimer?.invalidate()
        lanHealthTimer = nil
        lanHealth.clear()
        digestMaintenanceTimer?.invalidate()
        digestMaintenanceTimer = nil
        lastDigestAtByAddress.removeAll()
        currentLanEndpoint = nil
        currentLanInstanceToken = nil
        currentLanNetworkId = nil
        stopMeshRoles()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        MeshRouter.unregisterLan()
        MeshRouter.reset()
        MeshConnectivityStatus.shared.clear()
        relayTimer?.invalidate()
        relayTimer = nil
        relayRateLimitedUntilMs = 0
        lastKnownPushHealthy = nil
        pathMonitor?.cancel()
        pathMonitor = nil
        relayPushClient.stop()
        relayCancellable?.cancel()
        relayCancellable = nil
        relaySyncPending = false
        MeshRuntimeStatus.shared.markStopped()
        log.info("Mesh stopped")
    }

    func setAppForeground(_ foreground: Bool) {
        let changed = appForeground != foreground
        appForeground = foreground
        lanTransport?.setForegroundActive(foreground)
        // Battery, 2026-07-21: lanHealthTimer and digestMaintenanceTimer are
        // foreground-only (see their docs) -- background execution windows
        // are kept alive by CoreBluetooth activity alone, and neither tick
        // does anything BLE frame relay needs while backgrounded. Re-running
        // both start*Loop functions on any foreground/background flip stops
        // them immediately on backgrounding and, on returning to foreground,
        // fires an immediate catch-up tick (via their own guard) before
        // resuming the normal interval -- see startLanHealthLoop /
        // startDigestMaintenanceLoop. relayTimer is different: it must keep
        // running in the background (see RelayPushClient's class doc), but
        // RelayPollPolicy's backoff only ever applies while foregrounded
        // (see its doc), so a flip reschedules it immediately rather than
        // waiting out whatever long interval is already pending --
        // backgrounding hands the poll sole responsibility for relay
        // delivery.
        if changed, isRunning {
            startLanHealthLoop()
            startDigestMaintenanceLoop()
            reschedulePoll(currentlyHealthy: relayPushClient.isHealthy())
        }
    }

    // MARK: - Bluetooth audio coexistence

    private func registerBluetoothAudioObserver() {
        guard audioRouteObserver == nil else { return }
        audioRouteObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                self?.refreshBluetoothAudioState(reason: "route change")
            }
        }
    }

    private func unregisterBluetoothAudioObserver() {
        if let audioRouteObserver {
            NotificationCenter.default.removeObserver(audioRouteObserver)
            self.audioRouteObserver = nil
        }
    }

    /**
     Records whether Bluetooth audio is routed. It no longer changes the mesh:
     Android dropped that policy on 2026-07-09 because messaging was dead on a
     phone whenever earbuds were connected, and iOS has strictly less control
     over its radio than Android does, so there is no iOS-specific knob whose
     absence would justify a stricter rule here.
     */
    private func refreshBluetoothAudioState(reason: String) {
        guard isRunning else { return }
        switch bluetoothAudioBackoff.update(bluetoothAudioActive: isBluetoothAudioActive()) {
        case .audioClear:
            bluetoothAudioConnected = false
            log.info("Bluetooth audio route cleared (\(reason, privacy: .public))")
        case .audioConnected:
            bluetoothAudioConnected = true
            log.info("Bluetooth audio routed; mesh stays up (\(reason, privacy: .public))")
        case nil:
            return
        }
        MeshRuntimeStatus.shared.setBluetoothAudioConnected(bluetoothAudioConnected)
    }

    /// Active Bluetooth audio route (A2DP / HFP / LE audio). See `BluetoothAudioBackoff`.
    private func isBluetoothAudioActive() -> Bool {
        let outputs = AVAudioSession.sharedInstance().currentRoute.outputs
        return outputs.contains { port in
            switch port.portType {
            case .bluetoothA2DP, .bluetoothHFP, .bluetoothLE:
                return true
            default:
                return false
            }
        }
    }

    private func startMeshRoles() {
        guard !meshRolesRunning else { return }
        transport.start()
        meshRolesRunning = true
    }

    private func stopMeshRoles() {
        guard meshRolesRunning else { return }
        transport.stop()
        meshRolesRunning = false
        MeshRouter.resetBle()
    }

    func notifyChatViewed(chatId: Data) {
        guard let identity else { return }
        guard let contact = try? store.getContact(userId: chatId) else {
            notifyGroupViewed(groupId: chatId)
            return
        }
        let through = PeerStreamWatermark.through(store: store, chatId: chatId, senderUserId: chatId)
        guard through > 0 else { return }
        try? store.recordOutgoingReceipt(
            chatId: chatId,
            senderUserId: chatId,
            receiptType: ReceiptType.read,
            throughLamport: through
        )
        _ = queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.read,
            ackedSenderUserId: chatId,
            throughLamport: through
        )
        sendReceiptToContact(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.read,
            ackedSenderUserId: chatId,
            throughLamport: through
        )
        RelaySyncEvents.requestSync()
    }

    func notifyGroupViewed(groupId: Data) {
        guard let identity,
              let group = try? store.getGroup(groupId: groupId),
              group.memberUserIds.contains(identity.userId) else { return }
        for senderUserId in group.memberUserIds where senderUserId != identity.userId {
            let through = PeerStreamWatermark.through(
                store: store,
                chatId: groupId,
                senderUserId: senderUserId
            )
            guard through > 0 else { continue }
            try? store.recordOutgoingReceipt(
                chatId: groupId,
                senderUserId: senderUserId,
                receiptType: ReceiptType.read,
                throughLamport: through
            )
        }
    }

    // MARK: - Frames

    private func sendHello(address: String) {
        guard let identity else { return }
        MeshRouter.sendToAddress(address: address, frame: encodeHello(userId: identity.userId))
        // HELLO2 rides right behind the legacy HELLO: capability bits for
        // the hidden-kind spray bound. Pre-HELLO2 builds reject the unknown
        // frame type and drop it without touching the link.
        if let hello2 = try? encodeHello2(userId: identity.userId, capabilities: coreOwnCapabilities()) {
            MeshRouter.sendToAddress(address: address, frame: hello2)
        }
    }

    private func onFrameReceived(address: String, frame: Data) {
        guard let identity else { return }
        let parsed: Frame
        do {
            parsed = try parseFrame(bytes: frame)
        } catch {
            log.warning("Unparseable frame from \(address, privacy: .public)")
            return
        }
        switch parsed {
        case .hello(let userId):
            handleHello(address: address, userId: userId, identity: identity)
        case .hello2(let userId, let capabilities):
            MeshRouter.onHello2(address: address, userId: userId, capabilities: capabilities)
        case .envelope(let msgId, let hopTtl, let expiry, let recipientHint, let sealed):
            processInboundEnvelope(
                sourceAddress: address,
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed,
                identity: identity
            )
        case .digest(let chatId, let entries, let recentMsgIds):
            handleDigest(
                address: address,
                chatId: chatId,
                entries: entries,
                recentMsgIds: recentMsgIds,
                identity: identity
            )
        case .lanEndpoint(let instanceToken, let host, let port):
            handleLanEndpointHint(
                address: address,
                instanceToken: instanceToken,
                endpoint: LanManualEndpoint(host: host, port: port)
            )
        case .transportProbe(let nonce, let response):
            handleTransportProbe(address: address, nonce: nonce, response: response)
        }
    }

    private func sendLanEndpointHint(address: String) {
        guard let endpoint = currentLanEndpoint,
              let instanceToken = currentLanInstanceToken,
              let frame = try? encodeLanEndpoint(
                instanceToken: instanceToken,
                host: endpoint.host,
                port: endpoint.port
              ) else { return }
        _ = MeshRouter.sendToAddress(address: address, frame: frame)
    }

    private func queueCurrentLanEndpoint(to userId: Data) {
        guard let identity,
              let endpoint = currentLanEndpoint,
              let instanceToken = currentLanInstanceToken,
              let networkId = currentLanNetworkId,
              LanCapabilityStore.isSupported(userId: userId),
              let contact = try? store.getContact(userId: userId) else { return }
        LanEndpointSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            endpoint: endpoint,
            instanceToken: instanceToken,
            networkId: networkId
        )
    }

    private func handleLanEndpointHint(
        address: String,
        instanceToken: Data,
        endpoint: LanManualEndpoint
    ) {
        guard let userId = MeshRouter.userIdFor(address: address),
              (try? store.getContact(userId: userId)) != nil else { return }
        LanCapabilityStore.markSupported(userId: userId)
        refreshLanCapableContacts()
        LanEndpointCache.save(
            networkId: currentLanNetworkId,
            userId: userId,
            endpoint: endpoint
        )
        queueCurrentLanEndpoint(to: userId)
        guard MeshRouter.transportFor(address: address) != .lan else { return }
        lanTransport?.connect(endpoint, remoteInstanceToken: instanceToken)
    }

    /// Pushes the contacts that have demonstrated LAN support into the
    /// transport, each with the millisecond that support was last seen. The
    /// automatic-scan gate keeps sweeping while any of them is not linked
    /// over LAN and its evidence is still recent.
    ///
    /// Blocked contacts are excluded, and a contact that no longer exists
    /// simply isn't in the list, so deleting or blocking someone lets the
    /// gate close. Also runs on the periodic LAN health tick, which is what
    /// makes that true without every screen having to say so.
    ///
    /// The reading itself is a contact-list query plus a stored value per
    /// contact, so it runs off the main actor (same pattern as the relay
    /// sync pass, and matching Android's move of this work onto its store
    /// executor). The transport's setter is queue-hopping and safe to call
    /// from there.
    private func refreshLanCapableContacts() {
        guard lanTransport != nil else { return }
        Task.detached(priority: .utility) { [weak self] in
            let capable = MeshController.lanCapableContacts()
            await MainActor.run { self?.lanTransport?.updateLanCapableContacts(capable) }
        }
    }

    private nonisolated static func lanCapableContacts() -> [Data: Int64] {
        let store = AppStore.get()
        let contacts = (try? store.listContacts()) ?? []
        let blocked = Set((try? store.listBlockedUsers()) ?? [])
        var capable: [Data: Int64] = [:]
        for contact in contacts where !blocked.contains(contact.userId) {
            if let lastSupportedAtMs = LanCapabilityStore.lastSupportedAtMs(userId: contact.userId) {
                capable[contact.userId] = lastSupportedAtMs
            }
        }
        return capable
    }

    /// The contact list changed (a contact was deleted, blocked, or
    /// unblocked), so anything derived from it needs rebuilding.
    func contactListChanged() {
        refreshLanCapableContacts()
    }

    private func handleTransportProbe(address: String, nonce: UInt64, response: Bool) {
        guard MeshRouter.transportFor(address: address) == .lan else { return }
        if response {
            let now = Int64(Date().timeIntervalSince1970 * 1_000)
            if let latency = lanHealth.response(address: address, nonce: nonce, nowMs: now) {
                LanTransportDiagnostics.shared.probeSucceeded(latencyMs: latency)
            }
        } else {
            _ = MeshRouter.sendToAddress(
                address: address,
                frame: encodeTransportProbe(nonce: nonce, response: true)
            )
        }
    }

    /// Battery, 2026-07-21: foreground-only (see `setAppForeground`). This
    /// only probes LAN links -- `probeLanLinks` filters
    /// `MeshRouter.identifiedRoutes()` down to `.lan` transport, and
    /// `LanTransport` itself already suspends its automatic-scan/discovery
    /// activity while backgrounded (`setForegroundActive`), so there is
    /// nothing background-live for this to usefully probe. Invalidates and
    /// returns (no timer) when backgrounded; fires an immediate catch-up
    /// probe before arming the repeating timer whenever this runs while
    /// foregrounded, so a foreground return doesn't wait out a stale 30s.
    private func startLanHealthLoop() {
        lanHealthTimer?.invalidate()
        lanHealthTimer = nil
        guard appForeground else { return }
        _ = probeLanLinks(manual: false)
        lanHealthTimer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.refreshLanCapableContacts()
                _ = self?.probeLanLinks(manual: false)
            }
        }
    }

    private func requestLanProbe() -> String? {
        probeLanLinks(manual: true)
    }

    private func probeLanLinks(manual: Bool) -> String? {
        let routes = MeshRouter.identifiedRoutes().filter { $0.transport == .lan }
        guard !routes.isEmpty else { return "No secure local Wi-Fi link is active yet" }
        if manual { LanTransportDiagnostics.shared.probeStarted() }
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        for route in routes {
            switch lanHealth.next(
                address: route.address,
                nowMs: now,
                nonce: UInt64.random(in: 1...UInt64.max)
            ) {
            case .send(let nonce):
                _ = MeshRouter.sendToAddress(
                    address: route.address,
                    frame: encodeTransportProbe(nonce: nonce, response: false)
                )
            case .wait:
                break
            case .close:
                lanTransport?.closeConnection(address: route.address)
                LanTransportDiagnostics.shared.probeFailed(
                    "The encrypted LAN link stopped responding and was reconnected"
                )
            }
        }
        return nil
    }

    private func handleHello(address: String, userId: Data, identity: Identity) {
        guard MeshRouter.onHello(address: address, userId: userId) else {
            log.warning("Dropping HELLO that conflicts with the authenticated link identity")
            return
        }
        MeshConnectivityStatus.shared.mergeLastSeen(
            userId: userId,
            seenAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )
        if (try? store.getContact(userId: userId)) != nil,
           let transport = MeshRouter.transportFor(address: address) {
            recordPeerConnection(userId: userId, transport: transport, kind: .connected)
        }
        log.info("HELLO from \(address, privacy: .public) \(UserIdHex.encode(userId), privacy: .public)")
        sendLanEndpointHint(address: address)
        queueCurrentLanEndpoint(to: userId)
        drainCarriedEnvelopesTo(address: address, peerUserId: userId)
        sendDigest(address: address, userId: userId, identity: identity)
        refreshNearby()
    }

    /// Encode and send the §7.3 digest for `address` and record the time so
    /// `checkDigestMaintenance` can re-run it on a long-lived link (D8). Called
    /// at HELLO time and on the periodic re-digest tick.
    private func sendDigest(address: String, userId: Data, identity: Identity) {
        let entries: [DigestEntry]
        if let contact = try? store.getContact(userId: userId) {
            entries = (try? store.chatDigest(chatId: contact.userId)) ?? []
        } else {
            entries = []
        }
        // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): the advertised list
        // now includes not just what we're still carrying for others but
        // also what we've recently consumed or authored ourselves, so a
        // mule still holding our envelope learns on this digest that we
        // already have it -- see `store.coreConfirmCarriedDeliveries`.
        let advertised = (try? store.coreDigestAdvertisedMsgIds()) ?? []
        guard let digest = try? encodeDigest(
            chatId: identity.userId,
            entries: entries,
            recentMsgIds: advertised
        ) else {
            log.warning("Could not encode DIGEST for \(address, privacy: .public)")
            return
        }
        MeshRouter.sendToAddress(address: address, frame: digest)
        lastDigestAtByAddress[address] = Int64(Date().timeIntervalSince1970 * 1_000)
    }

    /// Battery, 2026-07-21: foreground-only (see `setAppForeground`). This
    /// tick only *re-sends* our own digest on links idle past their jittered
    /// re-digest window (`checkDigestMaintenance` -> `sendDigest`) -- a
    /// convergence nudge for messages/receipts that arrived after the
    /// connect-time digest, not the delivery path itself. Actual envelope
    /// delivery over BLE (and LAN) is driven directly by received
    /// `.envelope` frames in `onFrameReceived` -> `processInboundEnvelope`,
    /// which fires from CoreBluetooth/Network.framework callbacks
    /// independent of this timer -- see that function. `handleHello` also
    /// already sends a fresh digest on every (re)connect, so a background
    /// BLE link that drops and reconnects still gets one on the reconnect;
    /// only an already-long-lived link's periodic *re*-digest is deferred
    /// until the app returns to foreground. Nothing correctness-critical for
    /// background BLE frame relay depends on this tick.
    ///
    /// Invalidates and returns (no timer) when backgrounded; fires an
    /// immediate catch-up check before arming the repeating timer whenever
    /// this runs while foregrounded, so a foreground return doesn't wait out
    /// a stale 60s -- semantics on links that are actually due are otherwise
    /// unchanged from before this gating.
    private func startDigestMaintenanceLoop() {
        digestMaintenanceTimer?.invalidate()
        digestMaintenanceTimer = nil
        guard appForeground else { return }
        checkDigestMaintenance()
        digestMaintenanceTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.checkDigestMaintenance()
            }
        }
    }

    /// D8: re-run the digest exchange on links that have stayed up past their
    /// jittered 3-5 min interval so a message/receipt that arrived after the
    /// connect-time digest still converges without a reconnect. Digests are
    /// idempotent, so over-calling is safe.
    private func checkDigestMaintenance() {
        guard let identity else { return }
        let routes = MeshRouter.identifiedRoutes()
        let active = Set(routes.map { $0.address })
        lastDigestAtByAddress = lastDigestAtByAddress.filter { active.contains($0.key) }
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        for route in routes {
            let last = lastDigestAtByAddress[route.address] ?? 0
            let seed = UInt64(bitPattern: Int64(truncatingIfNeeded: route.address.hashValue))
            if shouldRedigest(nowMs: now, lastDigestAtMs: last, jitterSeed: seed) {
                sendDigest(address: route.address, userId: route.userId, identity: identity)
            }
        }
    }

    private func handleDigest(
        address: String,
        chatId: Data,
        entries: [DigestEntry],
        recentMsgIds: [Data],
        identity: Identity
    ) {
        let peerUserId = MeshRouter.userIdFor(address: address)
        guard DigestSync.isExpectedChatId(digestChatId: chatId, helloUserId: peerUserId),
              let peerUserId else {
            log.warning("Dropping DIGEST from \(address, privacy: .public)")
            return
        }
        if let contact = try? store.getContact(userId: peerUserId) {
            syncReceiptsFirst(identity: identity, contact: contact, address: address, entries: entries)
            let peerHasThrough = DigestSync.throughLamportForSelf(entries: entries, ownUserId: identity.userId)
            let queued = (try? store.outboundEnvelopesAfter(
                chatId: contact.userId,
                senderUserId: identity.userId,
                afterLamport: peerHasThrough
            )) ?? []
            let byLamport = Dictionary(uniqueKeysWithValues: queued.map { ($0.lamport, $0) })
            // Same once-per-session bound as the core spray plan: a peer
            // without CAP_ACKS_HIDDEN_KINDS never advances its DELIVERED
            // watermark past hidden kinds, so this direct re-offer would
            // repeat them on every digest for the full expiry.
            let gateHidden = !MeshRouter.peerAcksHiddenKinds(address: address)
            let alreadyOffered = gateHidden
                ? Set(MeshRouter.hiddenOfferedFor(address: address))
                : Set<Data>()
            var newlyOffered: [Data] = []
            let missing = (try? store.messagesAfter(
                chatId: contact.userId,
                senderUserId: identity.userId,
                afterLamport: peerHasThrough
            )) ?? []
            for message in missing {
                let outbound = byLamport[message.lamport]
                    ?? backfillOutbound(identity: identity, contact: contact, message: message)
                if let outbound {
                    if gateHidden, coreIsHiddenSprayKind(kind: outbound.kind) {
                        if alreadyOffered.contains(outbound.msgId) { continue }
                        newlyOffered.append(outbound.msgId)
                    }
                    MeshRouter.sendToAddress(address: address, frame: encodeOutboundEnvelopeFrame(outbound))
                }
            }
            MeshRouter.recordHiddenOffered(address: address, msgIds: newlyOffered)
        }
        resendGroupOutboundToPeer(address: address, peerUserId: peerUserId, identity: identity)
        sprayDigestPlanTo(
            address: address,
            peerUserId: peerUserId,
            peerKnownIds: recentMsgIds,
            identity: identity
        )
    }

    /// DTN D4 (seen-set poisoning ordering, mirrors Android
    /// `MeshService.processInboundEnvelope`'s KDoc): [GossipState.seenIds]
    /// is checked with the non-mutating `contains`, never `checkAndRecord`,
    /// and only recorded once this envelope reaches a **terminal handled
    /// state** -- consumed, carried, or expired-drop -- at each `return`
    /// below. Invariant: an envelope whose durable handling failed must be
    /// re-presentable; an envelope that was handled (even by deliberate
    /// drop) must be deduped. Before this, `checkAndRecord` ran up front, so
    /// a later store failure (e.g. disk-full out of `carryForeign`)
    /// permanently poisoned the `msgId` even though it was never actually
    /// carried or delivered.
    ///
    /// Loop-hazard note (see `relayForeign`'s doc comment): recording after
    /// relaying is safe here because the arriving link is excluded from the
    /// relay fanout and this function runs synchronously per received frame,
    /// so this node cannot re-ingest the frame it just relayed before the
    /// `record` call below completes.
    func processInboundEnvelope(
        sourceAddress: String?,
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        identity: Identity
    ) -> CoreInboundDisposition {
        let sourceLabel = sourceAddress ?? "relay"
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        switch coreInboundGate(
            isNewMsgId: !GossipState.seenIds.contains(msgId: msgId),
            hopTtl: hopTtl,
            expiryMs: expiry,
            nowMs: now
        ) {
        case .seen:
            return .seen
        case .expired:
            log.info("Dropping expired envelope from \(sourceLabel, privacy: .public)")
            // A deliberate drop is still a terminal handled state.
            GossipState.seenIds.record(msgId: msgId)
            return .expired
        case .rejected:
            log.warning("Dropping envelope with invalid hop or expiry fields from \(sourceLabel, privacy: .public)")
            GossipState.seenIds.record(msgId: msgId)
            return .rejected
        case .dispatch:
            break
        }
        let opened: OpenedMessage
        do {
            opened = try openMessage(recipient: identity, sealed: sealed)
        } catch {
            // Pairwise open failed: either foreign 1:1 traffic, or a group
            // envelope sealed with a shared key (DESIGN.md §6.5). Try groups
            // whose recipient_hint matches before treating it as pure mule
            // traffic. Group members keep relaying/carrying so absent members
            // still get a copy.
            //
            // T4-06: this catch is deliberately scoped to `openMessage` ONLY.
            // A store failure while delivering a message that WAS ours must
            // not be misread here as "not for us, carry as foreign" -- the
            // own-delivery path below has its own catch that returns .failed.
            if let (group, opened) = tryOpenGroupMessage(
                recipientHint: recipientHint, ownUserId: identity.userId, sealed: sealed, now: now
            ) {
                do {
                    try deliverOpenedGroupEnvelope(
                        sourceLabel: sourceLabel,
                        group: group,
                        opened: opened,
                        identity: identity,
                        msgId: msgId,
                        arrival: messageArrival(
                            sourceAddress: sourceAddress,
                            senderUserId: opened.senderUserId,
                            receivedHopTtl: hopTtl
                        )
                    )
                } catch {
                    // T4-06: durable store of our own group copy failed. Leave
                    // re-presentable (no record) and never acked.
                    log.warning("Deferring group envelope from \(sourceLabel, privacy: .public): durable delivery failed")
                    return .failed
                }
                // specs/group-relay-durability.md §4.3 no-reinjection rule:
                // a relay-fetched group message addressed to OUR OWN hint is
                // a per-member fan-out copy -- the relay fan-out already
                // reaches every member durably, so re-flooding/carrying it
                // would give the same content a second flood identity under
                // the fan-out msgId. Legacy group-hint relay rows and every
                // BLE/LAN-sourced group frame keep the flood+carry behavior.
                // Mirrors MeshService.kt.
                let ownFanoutCopy = sourceAddress == nil &&
                    coreIsOwnFanoutHint(
                        recipientHint: recipientHint,
                        ownUserId: identity.userId,
                        nowMs: now
                    )
                if !ownFanoutCopy {
                    relayForeign(
                        sourceAddress: sourceAddress,
                        msgId: msgId,
                        hopTtl: hopTtl,
                        expiry: expiry,
                        recipientHint: recipientHint,
                        sealed: sealed
                    )
                    _ = carryForeign(
                        msgId: msgId,
                        hopTtl: hopTtl,
                        expiry: expiry,
                        recipientHint: recipientHint,
                        sealed: sealed,
                        forceFamily: true
                    )
                }
                // DTN D4: we already durably delivered our own copy above
                // (`deliverOpenedGroupEnvelope`), so record regardless of
                // whether the best-effort mule copy for absent members
                // succeeded -- same reasoning as Android's KDoc.
                GossipState.seenIds.record(msgId: msgId)
                return .consumed
            }
            relayForeign(
                sourceAddress: sourceAddress,
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            )
            let carried = carryForeign(
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            )
            // DTN D4: only record once the durable carry actually succeeded.
            // `carryForeign` reports store failure via its Bool return
            // (rather than swallowing it silently), so a disk-full failure
            // here leaves this msgId unrecorded: the next copy of this
            // envelope on any link re-gates as `.dispatch` and gets another
            // chance to carry it, instead of being silently dropped as
            // `.seen` for the rest of the process lifetime.
            if carried {
                GossipState.seenIds.record(msgId: msgId)
            }
            return .carried
        }

        // `openMessage` succeeded: this envelope is ours. Delivering it is a
        // separate do/catch from the open above so a store failure here is
        // reported as `.failed` (re-presentable, never acked) rather than
        // being mistaken for foreign traffic (T4-06).
        let arrival = messageArrival(
            sourceAddress: sourceAddress,
            senderUserId: opened.senderUserId,
            receivedHopTtl: hopTtl
        )
        let consumedKind: UInt8?
        do {
            consumedKind = try deliverOpened(
                sourceLabel: sourceLabel,
                sourceAddress: sourceAddress,
                opened: opened,
                identity: identity,
                msgId: msgId,
                arrival: arrival
            )
        } catch {
            log.warning("Deferring envelope from \(sourceLabel, privacy: .public): durable delivery failed")
            return .failed
        }
        // The ONE place this device may vouch for a hidden kind's relay copy:
        // reaching here means `openMessage` succeeded against our own identity
        // key (so the envelope was pairwise-sealed to us and nobody else can
        // open it) and delivery ran to completion. Both consumption paths pass
        // through here -- BLE/LAN frames and relay-fetched envelopes alike --
        // so a relay-consumed hidden kind is equally re-ackable if the mailbox
        // ever re-presents it. Mirrors InboundEnvelopeProcessor.kt; see
        // `coreRecordConsumedHiddenMsgId` for every condition core re-checks
        // and for why anything unprovable must not be recorded.
        if let consumedKind {
            recordConsumedHiddenKind(
                msgId: msgId,
                kind: consumedKind,
                recipientHint: recipientHint,
                expiry: expiry,
                identity: identity,
                now: now
            )
        }
        // DTN D4: delivery ran to completion -- safe, and required, to record.
        GossipState.seenIds.record(msgId: msgId)
        return .consumed
    }

    /// Best-effort note that this device consumed this envelope as its sole
    /// true endpoint consumer, so a later relay copy of the same `msgId` can
    /// be acked away instead of sitting in the mailbox until expiry.
    ///
    /// Deliberately swallows store failures: a missing record costs one relay
    /// re-fetch, which is precisely the cost this mechanism trades against,
    /// and must never turn into a failed delivery. Core owns every safety
    /// condition (kind, own-hint, group-hint, expiry) and simply declines to
    /// write a row when one doesn't hold, so this call site's only job is to
    /// be reached exclusively from the proven-consumption path above.
    private func recordConsumedHiddenKind(
        msgId: Data,
        kind: UInt8,
        recipientHint: Data,
        expiry: Int64,
        identity: Identity,
        now: Int64
    ) {
        _ = try? store.coreRecordConsumedHiddenMsgId(
            msgId: msgId,
            kind: kind,
            recipientHint: recipientHint,
            expiryMs: expiry,
            ownUserId: identity.userId,
            nowMs: now
        )
    }

    private func messageArrival(
        sourceAddress: String?,
        senderUserId: Data,
        receivedHopTtl: UInt8
    ) -> MessageArrival {
        let transport: UInt8
        if let sourceAddress {
            let linkPeerMatchesSender = MeshRouter.userIdFor(address: sourceAddress) == senderUserId
            if MeshRouter.transportFor(address: sourceAddress) == .lan {
                transport = linkPeerMatchesSender ? 3 : 4
            } else {
                transport = linkPeerMatchesSender ? 0 : 1
            }
        } else {
            transport = 2
        }
        let hopsTaken = arrivalHopsTaken(receivedHopTtl: receivedHopTtl)
        return MessageArrival(
            transport: transport,
            hopsTaken: hopsTaken,
            receivedAt: Int64(Date().timeIntervalSince1970 * 1_000)
        )
    }

    /// Opens `sealed` with any imported group `groupOpenCandidates` offers
    /// for `recipientHint`: groups whose own recent-day hints match, plus
    /// every imported group when the hint is OUR OWN -- a per-member relay
    /// fan-out copy (specs/group-relay-durability.md §4.1) is addressed to
    /// the member, not the group, so nothing but the group key identifies
    /// it. Returns the matching group and opened payload, or nil.
    /// `openGroupMessage` does not check membership of the signer; callers
    /// must enforce that before trusting the body. Mirrors
    /// InboundEnvelopeProcessor.kt.
    private func tryOpenGroupMessage(
        recipientHint: Data,
        ownUserId: Data,
        sealed: Data,
        now: Int64
    ) -> (Group, OpenedMessage)? {
        let groups = (try? store.groupOpenCandidates(
            hint: recipientHint, ownUserId: ownUserId, nowMs: now
        )) ?? []
        for group in groups {
            if let opened = try? openGroupMessage(group: group, sealed: sealed) {
                return (group, opened)
            }
        }
        return nil
    }

    /// Returns the body's `kind` once it is known, or `nil` if the body could
    /// not even be decoded. Every other early return still reports its kind:
    /// a deliberate discard (blocked sender, unauthorized sender, unhandled
    /// kind) is consumption by an endpoint that is finished with the envelope,
    /// which is exactly what `processInboundEnvelope` treats as `.consumed`
    /// and what may be recorded as a consumed hidden kind. Only "we could not
    /// tell what this was" withholds that. Mirrors
    /// InboundEnvelopeProcessor.kt's `deliverOpenedEnvelope`.
    @discardableResult
    private func deliverOpened(
        sourceLabel: String,
        sourceAddress: String?,
        opened: OpenedMessage,
        identity: Identity,
        msgId: Data,
        arrival: MessageArrival
    ) throws -> UInt8? {
        let extendedBody: ExtendedMessageBody
        do {
            extendedBody = try decodeExtendedMessageBody(bytes: opened.payload)
        } catch {
            // Undecodable body from a verified sender: deterministic reject,
            // terminal handled state (not a store failure).
            return nil
        }
        let body = MessageBody(
            kind: extendedBody.kind,
            chatId: extendedBody.chatId,
            lamport: extendedBody.lamport,
            timestamp: extendedBody.timestamp,
            content: extendedBody.content
        )
        guard body.chatId == opened.senderUserId else { return body.kind }
        let senderIsContact = (try? store.getContact(userId: opened.senderUserId)) != nil
        guard corePairwiseSenderAuthorized(
            kind: body.kind,
            senderIsContact: senderIsContact,
            senderIsSelf: opened.senderUserId == identity.userId
        ) else {
            log.warning("Dropping pairwise envelope from unauthorized sender on \(sourceLabel, privacy: .public)")
            return body.kind
        }

        // Blocked identities are dropped before ANY kind handler runs: a
        // replayed kind=3 must not resurrect the contact, no receipts are
        // authored (the blocked party sees nothing), and the relay copy still
        // acks away as consumed — we are the sole endpoint and deliberate
        // discard is consumption, so the mailbox doesn't refetch it forever.
        if (try? store.isUserBlocked(userId: opened.senderUserId)) == true {
            log.info("Dropping envelope from blocked sender on \(sourceLabel, privacy: .public)")
            return body.kind
        }

        switch body.kind {
        case ProtocolKind.text, ProtocolKind.attachmentManifest, ProtocolKind.reaction:
            try handleIncomingChat(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity,
                kind: body.kind,
                msgId: msgId,
                replyToMsgId: extendedBody.replyToMsgId,
                arrival: arrival
            )
        case ProtocolKind.receipt:
            try handleIncomingReceipt(
                sourceAddress: sourceAddress,
                envelopeSender: opened.senderUserId,
                body: body,
                identity: identity,
                arrival: arrival
            )
        case ProtocolKind.friendRequest:
            try handleIncomingFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.profileSync:
            try handleIncomingProfileSync(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.friendDirectory:
            try handleIncomingFriendDirectory(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.introducedFriendRequest:
            try handleIncomingIntroducedFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.lanEndpointHint:
            try handleIncomingLanEndpointHint(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.relayUpdate:
            try handleIncomingRelayUpdate(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.groupInvite:
            try handleIncomingGroupInvite(
                sourceLabel: sourceLabel,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        default:
            log.info("Unhandled kind=\(body.kind) from \(sourceLabel, privacy: .public)")
        }
        return body.kind
    }

    /// Delivers a group-sealed envelope we opened with an imported group key
    /// (DESIGN.md §6.5). Wire `MessageBody.chatId` is the group id; the
    /// verified signer must be a current member (core does not check this).
    /// Group receipts are deferred — we only store + notify.
    private func deliverOpenedGroupEnvelope(
        sourceLabel: String,
        group: Group,
        opened: OpenedMessage,
        identity: Identity,
        msgId: Data,
        arrival: MessageArrival?
    ) throws {
        guard group.memberUserIds.contains(opened.senderUserId) else {
            log.warning("Dropping group envelope from \(sourceLabel, privacy: .public): signer is not a member of \(group.name, privacy: .public)")
            return
        }
        guard group.memberUserIds.contains(identity.userId) else {
            log.warning("Dropping group envelope from \(sourceLabel, privacy: .public): we are not a member of \(group.name, privacy: .public)")
            return
        }
        let extendedBody: ExtendedMessageBody
        do {
            extendedBody = try decodeExtendedMessageBody(bytes: opened.payload)
        } catch {
            return
        }
        let body = MessageBody(
            kind: extendedBody.kind,
            chatId: extendedBody.chatId,
            lamport: extendedBody.lamport,
            timestamp: extendedBody.timestamp,
            content: extendedBody.content
        )
        guard body.chatId == group.id else {
            log.warning("Dropping group envelope from \(sourceLabel, privacy: .public): body.chatId does not match group id")
            return
        }
        switch body.kind {
        case ProtocolKind.text, ProtocolKind.attachmentManifest, ProtocolKind.reaction:
            try handleIncomingGroupChatMessage(
                group: group,
                senderUserId: opened.senderUserId,
                body: body,
                msgId: msgId,
                replyToMsgId: extendedBody.replyToMsgId,
                arrival: arrival
            )
        case ProtocolKind.groupMetadataUpdate:
            try handleIncomingGroupMetadataUpdate(
                sourceLabel: sourceLabel,
                group: group,
                senderUserId: opened.senderUserId,
                body: body,
                msgId: msgId,
                replyToMsgId: extendedBody.replyToMsgId,
                arrival: arrival
            )
        default:
            log.info("Dropping group envelope from \(sourceLabel, privacy: .public): unhandled kind=\(body.kind)")
        }
    }

    private func handleIncomingGroupMetadataUpdate(
        sourceLabel: String,
        group: Group,
        senderUserId: Data,
        body: MessageBody,
        msgId: Data,
        replyToMsgId: Data?,
        arrival: MessageArrival?
    ) throws {
        let updated: Group?
        do {
            let update = try decodeGroupMetadataUpdate(bytes: body.content)
            updated = try applyGroupMetadataUpdate(
                group: group,
                update: update,
                senderUserId: senderUserId
            )
        } catch {
            // Deterministic reject (bad/inapplicable metadata) -- terminal
            // handled state, distinct from a store failure. Swallow here so
            // it is NOT reported as .failed by the caller.
            log.warning("Dropping invalid group metadata from \(sourceLabel, privacy: .public)")
            return
        }
        // T4-06: primary store failure propagates (see handleIncomingChat).
        let inserted = try store.insertIncomingMessage(
            message: StoredMessage(
                chatId: group.id,
                senderUserId: senderUserId,
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: body.kind,
                payload: body.content
            ),
            msgId: msgId,
            replyToMsgId: replyToMsgId
        )
        guard inserted else { return }
        if let arrival {
            _ = try? store.recordMessageArrival(
                chatId: group.id,
                senderUserId: senderUserId,
                lamport: body.lamport,
                arrival: arrival
            )
        }
        if let updated {
            do {
                try store.upsertGroup(group: updated)
                ChatEvents.notifyChatChanged(group.id)
            } catch {
                log.error("Failed to persist group metadata revision \(updated.metadataRevision)")
            }
        }
    }

    private func handleIncomingGroupChatMessage(
        group: Group,
        senderUserId: Data,
        body: MessageBody,
        msgId: Data,
        replyToMsgId: Data?,
        arrival: MessageArrival?
    ) throws {
        // T4-06: primary store failure propagates (see handleIncomingChat).
        let inserted = try store.insertIncomingMessage(
            message: StoredMessage(
                chatId: group.id,
                senderUserId: senderUserId,
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: body.kind,
                payload: body.content
            ),
            msgId: msgId,
            replyToMsgId: replyToMsgId
        )
        guard inserted else { return }
        if let arrival {
            _ = try? store.recordMessageArrival(
                chatId: group.id,
                senderUserId: senderUserId,
                lamport: body.lamport,
                arrival: arrival
            )
        }
        recordInboundChatArrival(senderUserId: senderUserId, kind: body.kind, arrival: arrival)
        ChatEvents.notifyChatChanged(group.id)

        // Local read watermark only (group wire receipts are deferred).
        let throughLamport = PeerStreamWatermark.through(
            store: store,
            chatId: group.id,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: group.id,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: throughLamport
        )
        if ChatVisibility.isVisible(group.id) {
            try? store.recordOutgoingReceipt(
                chatId: group.id,
                senderUserId: senderUserId,
                receiptType: ReceiptType.read,
                throughLamport: throughLamport
            )
        } else if isVisibleChatKind(body.kind) {
            let senderName = (try? store.getContact(userId: senderUserId))
                .map { coreContactDisplayName(contact: $0) }
                ?? String(UserIdHex.encode(senderUserId).prefix(8))
            let preview = body.kind == ProtocolKind.attachmentManifest
                ? AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
                : (String(data: body.content, encoding: .utf8) ?? "")
            MessageNotifier.notifyIncomingGroupMessage(group: group, senderName: senderName, preview: preview)
        }
    }

    /// Imports a pairwise-sealed `kind=4` group invite (DESIGN.md §6.5). Wire
    /// `chatId` is the invite sender's userId (1:1 pairwise convention); the
    /// group id/key/members live in the invite content. Local history is stored
    /// under `chat_id = group.id`.
    private func handleIncomingGroupInvite(
        sourceLabel: String,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        let group: Group
        do {
            group = try decodeGroupInviteContent(bytes: body.content)
        } catch {
            log.warning("Dropping group invite from \(sourceLabel, privacy: .public): failed to decode")
            return
        }
        guard group.memberUserIds.contains(identity.userId) else {
            log.warning("Dropping group invite from \(sourceLabel, privacy: .public): we are not listed as a member")
            return
        }
        guard group.memberUserIds.contains(senderUserId) else {
            log.warning("Dropping group invite from \(sourceLabel, privacy: .public): sender is not listed as a member")
            return
        }

        // T4-06: persisting the group is the durable state that matters --
        // let a store failure propagate so the invite is not acked/deduped
        // and the group is not silently lost (previously this returned, which
        // the caller treated as consumed and acked the relay copy away).
        try store.upsertGroup(group: group)
        deliverCarriedMessagesForImportedGroup(group: group, identity: identity)
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: group.id,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.groupInvite,
            payload: body.content
        ))
        guard inserted else { return }
        ChatEvents.notifyChatChanged(group.id)
        log.info("Imported group \(group.name, privacy: .public) from invite on \(sourceLabel, privacy: .public)")

        if !ChatVisibility.isVisible(group.id) {
            let senderName = (try? store.getContact(userId: senderUserId))
                .map { coreContactDisplayName(contact: $0) }
                ?? String(UserIdHex.encode(senderUserId).prefix(8))
            MessageNotifier.notifyIncomingGroupMessage(
                group: group,
                senderName: senderName,
                preview: "Added you to \(group.name)"
            )
        }
    }

    private func handleIncomingChat(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity,
        kind: UInt8,
        msgId: Data,
        replyToMsgId: Data?,
        arrival: MessageArrival
    ) throws {
        // T4-06: let a store failure propagate (do NOT `try?`-swallow it into
        // the same `false` a harmless duplicate returns). `processInboundEnvelope`
        // turns the throw into `.failed`, leaving the envelope re-presentable
        // and its relay copy un-acked. A `false` here is a real duplicate --
        // already durably stored -- so it stays a terminal (return) state.
        let inserted = try store.insertIncomingMessage(
            message: StoredMessage(
                chatId: senderUserId,
                senderUserId: senderUserId,
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: kind,
                payload: body.content
            ),
            msgId: msgId,
            replyToMsgId: replyToMsgId
        )
        guard inserted else { return }
        _ = try? store.recordMessageArrival(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            arrival: arrival
        )
        ChatEvents.notifyChatChanged(senderUserId)

        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        let visible = ChatVisibility.isVisible(senderUserId)
        if visible {
            try? store.recordOutgoingReceipt(
                chatId: senderUserId,
                senderUserId: senderUserId,
                receiptType: ReceiptType.read,
                throughLamport: through
            )
        }
        guard let contact = try? store.getContact(userId: senderUserId) else { return }
        recordInboundChatArrival(senderUserId: senderUserId, kind: kind, arrival: arrival)
        _ = queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.delivered,
            ackedSenderUserId: senderUserId,
            throughLamport: through
        )
        if visible {
            _ = queueOutgoingReceiptForRelay(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.read,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
            if visible {
                sendReceiptOnAddress(
                    identity: identity,
                    contact: contact,
                    address: sourceAddress,
                    receiptType: ReceiptType.read,
                    ackedSenderUserId: senderUserId,
                    throughLamport: through
                )
            }
        } else {
            sendReceiptToContact(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
            if visible {
                sendReceiptToContact(
                    identity: identity,
                    contact: contact,
                    receiptType: ReceiptType.read,
                    ackedSenderUserId: senderUserId,
                    throughLamport: through
                )
            }
        }
        if !visible, isVisibleChatKind(kind) {
            let preview: String
            if kind == ProtocolKind.attachmentManifest {
                preview = AttachmentPayload.previewLabel(AttachmentPayload.decode(body.content))
            } else {
                preview = String(data: body.content, encoding: .utf8) ?? ""
            }
            MessageNotifier.notifyIncoming(contact: contact, preview: preview)
        }
        RelaySyncEvents.requestSync()
    }

    private func handleIncomingReceipt(
        sourceAddress: String?,
        envelopeSender: Data,
        body: MessageBody,
        identity: Identity,
        arrival: MessageArrival
    ) throws {
        guard let receipt = try? decodeReceiptContent(bytes: body.content) else { return }
        guard receipt.senderUserId == identity.userId else { return }
        guard (try? store.getContact(userId: envelopeSender)) != nil else { return }
        // T4-06: advancing the receipt watermark is the durable state here;
        // let a store failure propagate so a relay-fetched receipt is not
        // acked away before it is recorded. T6: the receipt returned on the
        // exact link that delivered the message -- record that route against
        // the watermark so every acked message's Info pane can prove the
        // BLE/LAN/relay round trip, not just the one at the watermark lamport.
        try store.recordReceipt(
            chatId: envelopeSender,
            senderUserId: identity.userId,
            receiptType: receipt.receiptType,
            throughLamport: receipt.lamport,
            viaTransport: arrival.transport
        )
        // V2 field metric: stamp delivery latency + route on the messages this
        // cumulative delivery receipt confirms.
        if receipt.receiptType == ReceiptType.delivered {
            try? store.recordDeliveredMetric(
                chatId: envelopeSender,
                throughLamport: receipt.lamport,
                deliveredAtMs: arrival.receivedAt,
                viaTransport: arrival.transport
            )
            // .messageDelivered is the OUTBOUND direction: this receipt proves
            // a message *we* sent reached them. The inbound direction is
            // recorded in recordInboundChatArrival.
            try? store.recordPeerConnectionEvent(
                userId: envelopeSender,
                transport: corePeerTransportForArrival(transport: arrival.transport),
                kind: .messageDelivered,
                occurredAtMs: arrival.receivedAt
            )
        }
        ChatEvents.notifyChatChanged(envelopeSender)
    }

    // FI5: throws now (was fully swallowed) -- matches the T4-06 discipline
    // already used by handleIncomingChat/handleIncomingReceipt/
    // handleIncomingGroupInvite in this file. `deliverOpened`'s catch turns
    // this into `.failed`, leaving the relay copy un-acked and the envelope
    // re-presentable, instead of a transient store failure (disk-full, busy)
    // permanently destroying a friend request. The two store writes below
    // that matter for that guarantee -- `upsertImportedContact` (durably
    // adds the contact; the actual effect of "processing a friend request")
    // and `insertMessage` (the dedup gate, same shape as every other
    // handler here) -- now propagate; everything else (provenance,
    // suggestion cleanup, outbound profile-sync queueing, receipts) stays
    // best-effort `try?`, same as before.
    /// Was this peer in range when we accepted them? Recorded in
    /// `ContactProvenance.addedNearby` so the composer can stay quiet about
    /// nearby-only delivery for people we actually met, and say it plainly for
    /// people who only ever arrived over the internet.
    private func peerIsNearby(_ userId: Data) -> Bool {
        MeshConnectivityStatus.shared.nearbyPeerIds.contains(userId)
    }

    private func handleIncomingFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        let pending = ((try? store.listFriendSuggestions(
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )) ?? []).first { $0.state == 1 && $0.candidate.userId == senderUserId }
        // Deterministic reject: undecodable/mismatched friend card from a
        // verified sender. Not a store failure -- stays a swallowed terminal
        // state.
        guard let json = String(data: body.content, encoding: .utf8),
              let card = try? parseFriendCard(json: json),
              friendCardUserId(card: card) == senderUserId else { return }
        let wasKnown = (try? store.getContact(userId: senderUserId)) != nil
        let contact = Contact(
            userId: senderUserId,
            name: card.name,
            signPk: card.signPk,
            agreePk: card.agreePk,
            relayUrl: card.relayUrl,
            relayToken: card.relayToken
        )
        _ = try store.upsertImportedContact(contact: contact)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }
        try? store.upsertContactProvenance(provenance: ContactProvenance(
            userId: senderUserId,
            source: pending == nil ? 0 : 1,
            introducerUserId: pending?.introducerUserId,
            introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000),
            addedNearby: peerIsNearby(senderUserId)
        ))
        if pending != nil { try? store.removeFriendSuggestion(candidateUserId: senderUserId) }
        ProfileSyncSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName(),
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.friendRequest,
            payload: body.content
        ))
        guard inserted else { return }
        ChatEvents.notifyChatChanged(senderUserId)
        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
        if !wasKnown {
            FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
            FriendImportEvents.subject.send(FriendImportEvent(contact: contact, directBluetooth: sourceAddress != nil))
            MessageNotifier.notifyFriendAdded(contact: contact)
        }
        log.info("Imported contact \(contact.name, privacy: .public) from friend request")
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale. `insertMessage` is the only MessageStore write in
    // this handler (the LAN endpoint cache/connect calls above it are a
    // separate, best-effort local cache, not the durable record this
    // envelope's ack decision is gated on), so it's the primary write here.
    private func handleIncomingLanEndpointHint(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable hint. Not a
        // store failure -- stays a swallowed terminal state.
        guard let contact = try? store.getContact(userId: senderUserId),
              let hint = try? decodeLanEndpointContent(bytes: body.content),
              let networkId = String(data: hint.networkId, encoding: .utf8) else { return }
        let endpoint = LanManualEndpoint(host: hint.host, port: hint.port)
        LanCapabilityStore.markSupported(userId: senderUserId)
        refreshLanCapableContacts()
        LanEndpointCache.save(networkId: networkId, userId: senderUserId, endpoint: endpoint)
        queueCurrentLanEndpoint(to: senderUserId)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }

        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        // The network fingerprint is stored with the cached endpoint but
        // deliberately does NOT gate this dial: requiring an exact match
        // silently disabled fresh hints on routed multi-subnet LANs -- the
        // case the sealed hint exists for (mDNS is link-local; TCP may
        // still route). A cross-network false positive is one bounded TCP
        // attempt to an endpoint the contact sealed to us, and Noise
        // authenticates. Being on some Wi-Fi is the only requirement.
        if hint.expiresAtMs > now, currentLanNetworkId != nil {
            lanTransport?.connect(endpoint, remoteInstanceToken: hint.instanceToken)
        }

        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.lanEndpointHint,
            payload: body.content
        ))
        if inserted {
            acknowledgeHiddenMessage(
                sourceAddress: sourceAddress,
                senderUserId: senderUserId,
                identity: identity,
                contact: contact
            )
        }
    }

    /// T23: a contact told us their own relay endpoint changed.
    ///
    /// `senderUserId` is the identity core verified sealed this envelope, and
    /// it is the only user id passed to `applyContactRelayUpdate` — core
    /// rejects the notice outright if its payload claims a different subject,
    /// so a notice can only ever move its own sender's endpoint, never a third
    /// party's. `insertMessage` is the dedup gate, same shape as every other
    /// handler in this file.
    private func handleIncomingRelayUpdate(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable content. Not a
        // store failure -- stays a swallowed terminal state.
        guard let contact = try? store.getContact(userId: senderUserId),
              let content = try? decodeRelayUpdateContent(bytes: body.content) else { return }
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.relayUpdate,
            payload: body.content
        ))
        guard inserted else { return }

        // A mis-scoped subject or a non-deposit credential throws: a
        // deterministic reject, not a store failure. The message row above
        // still stands so the sender's watermark advances and they stop
        // re-spraying it.
        let applied = (try? store.applyContactRelayUpdate(
            senderUserId: senderUserId,
            content: content
        )) ?? false
        if applied {
            log.info("Applied a relay update from \(contact.name, privacy: .public)")
            // Anything queued for them was addressed to the old endpoint's
            // mailbox; a sync pass re-resolves and re-posts to the new one.
            RelaySyncEvents.requestSync()
            ChatEvents.notifyChatChanged(senderUserId)
        }
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale. `insertMessage` is the dedup gate here, same shape
    // as every other handler in this file; the profile-content writes below
    // it (avatar/name/policy) stay best-effort `try?`, unchanged.
    private func handleIncomingProfileSync(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable content. Not a
        // store failure -- stays a swallowed terminal state.
        guard let existing = try? store.getContact(userId: senderUserId),
              let content = try? decodeProfileSyncContent(bytes: body.content) else { return }
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.profileSync,
            payload: body.content
        ))
        guard inserted else { return }

        let policyChanged = (try? store.upsertContactDiscoveryPolicy(policy: ContactDiscoveryPolicy(
            userId: senderUserId,
            protocolVersion: content.friendsOfFriendsVersion,
            enabled: content.friendsOfFriendsEnabled,
            revision: content.friendsOfFriendsRevision
        ))) ?? false

        let applied = (try? store.setContactAvatar(
            userId: senderUserId,
            avatar: content.avatar.isEmpty ? nil : content.avatar,
            epoch: content.avatarEpoch
        )) ?? false
        if applied, content.name != existing.name {
            let updated = Contact(
                userId: existing.userId,
                name: content.name,
                signPk: existing.signPk,
                agreePk: existing.agreePk,
                relayUrl: existing.relayUrl,
                relayToken: existing.relayToken
            )
            try? store.upsertContact(contact: updated)
        }
        ChatEvents.notifyChatChanged(senderUserId)

        let contact = (try? store.getContact(userId: senderUserId)) ?? existing
        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        _ = queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.delivered,
            ackedSenderUserId: senderUserId,
            throughLamport: through
        )
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        } else {
            sendReceiptToContact(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
        if policyChanged {
            FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
        }
        RelaySyncEvents.requestSync()
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale. `insertMessage` is the dedup gate here, same shape
    // as every other handler in this file. `applyFriendDirectory` below
    // stays `try?` deliberately: its throw conflates a store failure with a
    // deterministic fail-closed ticket-check reject (see its doc comment),
    // and reclassifying an unrecoverable ticket rejection as `.failed` would
    // make it retry forever instead of being a swallowed terminal state.
    private func handleIncomingFriendDirectory(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: unknown sender or undecodable content. Not a
        // store failure -- stays a swallowed terminal state.
        guard let contact = try? store.getContact(userId: senderUserId),
              let content = try? decodeFriendDirectoryContent(bytes: body.content) else { return }
        let inserted = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.friendDirectory,
            payload: body.content
        ))
        guard inserted else { return }
        if FriendsOfFriendsStore.isEnabled() {
            // Introductions stay inside one Cruise Pass. A directory from an
            // introducer on somebody else's pass is applied as an *empty*
            // snapshot rather than ignored: the revision bookkeeping stays
            // identical, and it additionally clears whatever that introducer
            // supplied before this rule existed. A phone therefore heals on
            // its own next pass instead of waiting for every other phone in
            // the graph to update. Mirrors InboundEnvelopeProcessor.kt.
            var scoped = content
            if !FriendDirectoryScope.sharesOwnPass(contact, ownRelay: RelayConfigStore.load()) {
                log.info("Scoping out friend directory: introducer is on another pass")
                scoped = FriendDirectoryContent(
                    version: content.version,
                    revision: content.revision,
                    entries: []
                )
            }
            guard (try? store.applyFriendDirectory(
                introducerUserId: senderUserId,
                recipientUserId: identity.userId,
                content: scoped,
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
            )) != nil else { return }
            ChatEvents.notifyChatChanged(senderUserId)
        }
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
    }

    // FI5: throws now -- see handleIncomingFriendRequest's doc for the
    // general rationale; same two primary writes (`upsertImportedContact`,
    // `insertMessage`) propagate here for the same reason.
    private func handleIncomingIntroducedFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) throws {
        // Deterministic reject: feature disabled, undecodable request, or a
        // ticket that fails verification (fail-closed by design -- an
        // invalid/expired/mismatched ticket should never be retried into
        // success). Not a store failure -- stays a swallowed terminal state.
        guard FriendsOfFriendsStore.isEnabled(),
              let request = try? decodeIntroducedFriendRequest(bytes: body.content),
              let card = try? parseFriendCard(json: request.friendCardJson),
              friendCardUserId(card: card) == senderUserId,
              let introducer = try? store.getContact(userId: request.ticket.introducerUserId),
              (try? verifyIntroductionTicket(
                ticket: request.ticket,
                introducerSignPk: introducer.signPk,
                expectedCandidateUserId: identity.userId,
                expectedInviteeUserId: senderUserId,
                expectedCandidatePolicyRevision: FriendsOfFriendsStore.revision(),
                nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
              )) == true else { return }

        let wasKnown = (try? store.getContact(userId: senderUserId)) != nil
        let contact = Contact(
            userId: senderUserId,
            name: card.name,
            signPk: card.signPk,
            agreePk: card.agreePk,
            relayUrl: card.relayUrl,
            relayToken: card.relayToken
        )
        _ = try store.upsertImportedContact(contact: contact)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }
        try? store.upsertContactProvenance(provenance: ContactProvenance(
            userId: senderUserId,
            source: 1,
            introducerUserId: introducer.userId,
            introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000),
            addedNearby: peerIsNearby(senderUserId)
        ))
        try? store.removeFriendSuggestion(candidateUserId: senderUserId)
        _ = try store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.introducedFriendRequest,
            payload: body.content
        ))
        acknowledgeHiddenMessage(
            sourceAddress: sourceAddress,
            senderUserId: senderUserId,
            identity: identity,
            contact: contact
        )
        FriendRequestSender.sendMutualFriendRequest(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName()
        )
        ProfileSyncSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName(),
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        FriendDirectorySender.queueToAllContacts(store: store, identity: identity)
        ChatEvents.notifyChatChanged(senderUserId)
        if !wasKnown {
            FriendImportEvents.subject.send(FriendImportEvent(
                contact: contact,
                directBluetooth: sourceAddress != nil
            ))
            MessageNotifier.notifyFriendAdded(contact: contact)
        }
    }

    private func acknowledgeHiddenMessage(
        sourceAddress: String?,
        senderUserId: Data,
        identity: Identity,
        contact: Contact
    ) {
        let through = PeerStreamWatermark.through(
            store: store,
            chatId: senderUserId,
            senderUserId: senderUserId
        )
        try? store.recordOutgoingReceipt(
            chatId: senderUserId,
            senderUserId: senderUserId,
            receiptType: ReceiptType.delivered,
            throughLamport: through
        )
        if queueOutgoingReceiptForRelay(
            identity: identity,
            contact: contact,
            receiptType: ReceiptType.delivered,
            ackedSenderUserId: senderUserId,
            throughLamport: through
        ) {
            RelaySyncEvents.requestSync()
        }
        if let sourceAddress {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: sourceAddress,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        } else {
            sendReceiptToContact(
                identity: identity,
                contact: contact,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: senderUserId,
                throughLamport: through
            )
        }
    }

    private func deliverCarriedMessagesForImportedGroup(group: Group, identity: Identity) {
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let hints = recentHintsFor(userId: group.id, nowMs: now)
        let carried = (try? store.carriedEnvelopesForHints(hints: hints, nowMs: now)) ?? []
        for envelope in carried {
            guard let opened = try? openGroupMessage(group: group, sealed: envelope.sealed) else { continue }
            do {
                try deliverOpenedGroupEnvelope(
                    sourceLabel: "carry queue",
                    group: group,
                    opened: opened,
                    identity: identity,
                    msgId: envelope.msgId,
                    arrival: nil
                )
            } catch {
                // T4-06: best-effort drain -- a store failure must not abort
                // the loop. The carried envelope is left in place (this path
                // never removes it), so a later import/trigger retries it.
                log.warning("Deferring carried group message: durable delivery failed")
            }
        }
    }

    private func resendGroupOutboundToPeer(
        address: String,
        peerUserId: Data,
        identity: Identity
    ) {
        let groups = (try? store.listGroups()) ?? []
        for group in groups where group.memberUserIds.contains(peerUserId)
            && group.memberUserIds.contains(identity.userId) {
            let envelopes = (try? store.outboundEnvelopesAfter(
                chatId: group.id,
                senderUserId: identity.userId,
                afterLamport: 0
            )) ?? []
            for envelope in envelopes {
                if envelope.kind == ProtocolKind.groupInvite,
                   envelope.recipientUserId != peerUserId {
                    continue
                }
                _ = MeshRouter.sendToAddress(
                    address: address,
                    frame: encodeOutboundEnvelopeFrame(envelope)
                )
            }
        }
    }

    // MARK: - Receipts / carry / relay

    private func syncReceiptsFirst(
        identity: Identity,
        contact: Contact,
        address: String,
        entries: [DigestEntry]
    ) {
        let peerAuthoredThrough = DigestSync.throughLamportForSender(entries: entries, senderUserId: contact.userId)
        guard peerAuthoredThrough > 0 else { return }
        let deliveredThrough = min(
            (try? store.outgoingReceiptThrough(
                chatId: contact.userId,
                senderUserId: contact.userId,
                receiptType: ReceiptType.delivered
            )) ?? 0,
            peerAuthoredThrough
        )
        if deliveredThrough > 0 {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: address,
                receiptType: ReceiptType.delivered,
                ackedSenderUserId: contact.userId,
                throughLamport: deliveredThrough
            )
        }
        let readThrough = min(
            (try? store.outgoingReceiptThrough(
                chatId: contact.userId,
                senderUserId: contact.userId,
                receiptType: ReceiptType.read
            )) ?? 0,
            peerAuthoredThrough
        )
        if readThrough > 0 {
            sendReceiptOnAddress(
                identity: identity,
                contact: contact,
                address: address,
                receiptType: ReceiptType.read,
                ackedSenderUserId: contact.userId,
                throughLamport: readThrough
            )
        }
    }

    private func sendReceiptOnAddress(
        identity: Identity,
        contact: Contact,
        address: String,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) {
        guard let authored = try? store.ensureAuthoredReceipt(
            identity: identity,
            contact: contact,
            ackedSenderUserId: ackedSenderUserId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        MeshRouter.sendToAddress(address: address, frame: authored.frame)
    }

    private func sendReceiptToContact(
        identity: Identity,
        contact: Contact,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) {
        guard let authored = try? store.ensureAuthoredReceipt(
            identity: identity,
            contact: contact,
            ackedSenderUserId: ackedSenderUserId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        _ = MeshRouter.sendToUserId(userId: contact.userId, frame: authored.frame)
    }

    @discardableResult
    private func queueOutgoingReceiptForRelay(
        identity: Identity,
        contact: Contact,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) -> Bool {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let existing = try? store.outgoingReceiptEnvelope(
            chatId: contact.userId,
            senderUserId: ackedSenderUserId,
            receiptType: receiptType
        )
        guard let authored = try? store.ensureAuthoredReceipt(
            identity: identity,
            contact: contact,
            ackedSenderUserId: ackedSenderUserId,
            receiptType: receiptType,
            throughLamport: throughLamport,
            timestampMs: timestamp
        ) else { return false }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        return existing == nil || existing!.throughLamport < authored.envelope.throughLamport
    }

    private func backfillOutbound(identity: Identity, contact: Contact, message: StoredMessage) -> OutboundEnvelope? {
        guard let authored = try? store.backfillPairwiseEnvelope(
            identity: identity,
            contact: contact,
            message: message,
            replyToMsgId: nil
        ) else { return nil }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        return authored.envelope
    }

    /// Android `carryForeignEnvelope` twin. Returns `true` if the store
    /// operation completed (whether it newly queued the envelope or found it
    /// already carried) and `false` if the store call itself failed (`try?`
    /// turns a thrown error into `nil`). DTN D4: `processInboundEnvelope`
    /// uses this return value to decide whether it's safe to mark the
    /// envelope's `msgId` seen -- see its doc comment.
    ///
    /// Also the only carry-ingest path on iOS today: relay proxy-fetched
    /// envelopes (FI2, `sourceAddress == nil`) reach `processInboundEnvelope`
    /// and fall into this same function -- there is no iOS twin of Android's
    /// separate `carryRelayEnvelope`/`enqueueRelayCarriedEnvelope` yet, so
    /// `carriedHopTtl` below covers both cases in one place.
    ///
    /// The stored `hopTtl` is `carriedHopTtl` of the received value, not the
    /// value verbatim -- see its doc comment for the full rationale and the
    /// zero-TTL saturation guarantee.
    private func carryForeign(
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        forceFamily: Bool = false
    ) -> Bool {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let isFamily = forceFamily || ((try? store.hintMatchesKnownTarget(hint: recipientHint, nowMs: now)) ?? false)
        guard let stored = try? store.enqueueCarriedEnvelope(
            envelope: CarriedEnvelope(
                msgId: msgId,
                hopTtl: carriedHopTtl(hopTtl),
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            ),
            isFamily: isFamily,
            receivedAtMs: now,
            foreignBudgetBytes: MeshDefaults.foreignCarryBudgetBytes
        ) else {
            return false
        }
        if stored, isFamily {
            RelaySyncEvents.requestSync()
        }
        return true
    }

    /// Floods a foreign (not-for-us) envelope onward, Android
    /// `relayForeignEnvelope` twin. The arriving link is excluded from the
    /// fanout below to avoid the trivial echo.
    ///
    /// DTN D4 loop-hazard note: since `processInboundEnvelope` moved to
    /// check-then-record, `GossipState.seenIds` is *not yet* updated for
    /// this `msgId` at the moment this call happens (it's recorded after
    /// this function returns, once the whole terminal branch succeeds -- see
    /// `processInboundEnvelope`'s doc comment). That's still safe against
    /// self-re-ingestion: the arriving link is excluded from the fanout (so
    /// this node can't hand the relayed frame straight back to itself), and
    /// `processInboundEnvelope` runs synchronously per received frame, so
    /// there is no way for this same `msgId` to re-enter
    /// `processInboundEnvelope` on *this* node before the terminal `record`
    /// call a few lines below this call site completes. A frame this node
    /// relays could only loop back from a third node's rebroadcast, which
    /// takes at least one more hop and one more link round-trip -- by then
    /// this node's record has long since happened.
    private func relayForeign(
        sourceAddress: String?,
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data
    ) {
        let remaining = Int(hopTtl)
        guard remaining > 1 else { return }
        let frame = encodeEnvelopeFrame(
            msgId: msgId,
            hopTtl: UInt8(remaining - 1),
            expiry: expiry,
            recipientHint: recipientHint,
            sealed: sealed
        )
        if let sourceAddress {
            _ = MeshRouter.relayToAllExcept(sourceAddress, frame: frame)
        } else {
            _ = MeshRouter.relayToAll(frame: frame)
        }
    }

    /// Hands over every carried envelope destined for the peer that just
    /// HELLO'd on `address` (DESIGN.md §5.3): compute the peer's recent-day
    /// `recipient_hint`s (`deliveryHintsForPeer`) and pull matching envelopes
    /// from the store, and send each on this link. Expired entries are
    /// pruned first.
    ///
    /// `env.hopTtl` here is forwarded verbatim -- it's already
    /// `carriedHopTtl` of what this device originally received, decremented
    /// once at `carryForeign` enqueue time, not the raw value the frame
    /// arrived with. No further decrement happens here.
    ///
    /// DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): this function only
    /// ever *attempts* delivery -- it no longer calls
    /// `store.removeCarriedEnvelope` on a successful
    /// `MeshRouter.sendToAddress`. That return only means a transport
    /// function accepted the write, not that the bytes made it to the peer;
    /// a disconnect mid-transfer used to silently drop the whole write
    /// queue after we'd already deleted our only copy. The carried row is
    /// now removed later, once the peer's own next digest exchange proves
    /// they actually have it -- see `store.coreConfirmCarriedDeliveries`,
    /// called from `sprayDigestPlanTo`.
    ///
    /// Invariant, stated verbatim (DTN_TODOS.md §3.2): worst case of a
    /// dropped mid-transfer link is a harmless duplicate resend (the peer's
    /// seen-set/store dedupes it), never a lost envelope; an unconfirmed
    /// carry still dies at its normal expiry via `store.pruneExpiredCarried`.
    private func drainCarriedEnvelopesTo(address: String, peerUserId: Data) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        try? store.pruneExpiredCarried(nowMs: now)
        let hints = (try? store.deliveryHintsForPeer(peerUserId: peerUserId, nowMs: now)) ?? []
        let toDeliver = (try? store.carriedEnvelopesForHints(hints: hints, nowMs: now)) ?? []
        var delivered = 0
        for env in toDeliver {
            let frame = encodeEnvelopeFrame(
                msgId: env.msgId,
                hopTtl: env.hopTtl,
                expiry: env.expiry,
                recipientHint: env.recipientHint,
                sealed: env.sealed
            )
            if MeshRouter.sendToAddress(address: address, frame: frame) {
                delivered += 1
            }
        }
        if delivered > 0 {
            log.info("Attempted delivery of \(delivered) carried envelope(s) to \(address, privacy: .public) (removal awaits their digest confirmation)")
        }
    }

    private func sprayDigestPlanTo(
        address: String,
        peerUserId: Data,
        peerKnownIds: [Data],
        identity: Identity
    ) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        // DTN D2 mule-drain-confirm (DTN_TODOS.md §3.2): confirm delivery of
        // anything this digest's advertised msg_ids prove the peer already
        // has BEFORE building the spray plan below, so a just-confirmed
        // carried envelope isn't immediately re-sprayed back at the peer who
        // just told us they have it.
        if let confirmed = try? store.coreConfirmCarriedDeliveries(
            peerUserId: peerUserId,
            peerKnownMsgIds: peerKnownIds,
            nowMs: now
        ), confirmed > 0 {
            log.info("Confirmed delivery of \(confirmed) carried envelope(s) to \(UserIdHex.encode(peerUserId), privacy: .public); dropped our copy")
        }
        guard let plan = try? store.coreDigestSprayPlan(
            ownUserId: identity.userId,
            peerUserId: peerUserId,
            peerHints: recentHintsFor(userId: peerUserId, nowMs: now),
            peerKnownMsgIds: peerKnownIds,
            nowMs: now,
            ownOutboundBudgetBytes: MeshDefaults.ownOutboundSprayBudgetBytes,
            ownReceiptBudgetBytes: MeshDefaults.ownReceiptSprayBudgetBytes,
            receiptQueryLimit: MeshDefaults.relayStoreBatchLimit,
            peerAcksHiddenKinds: MeshRouter.peerAcksHiddenKinds(address: address),
            hiddenAlreadyOffered: MeshRouter.hiddenOfferedFor(address: address)
        ) else {
            log.warning("Failed to build digest spray plan for \(address, privacy: .public)")
            return
        }
        let frames = plan.carriedFrames + plan.ownOutboundFrames + plan.ownReceiptFrames
        for frame in frames {
            _ = MeshRouter.sendToAddress(address: address, frame: frame)
        }
        MeshRouter.recordHiddenOffered(address: address, msgIds: plan.offeredHiddenMsgIds)
    }

    // Hint aggregation (recent/delivery/known-target/group matching) moved
    // into the Rust core (FA15 follow-up) -- see core/src/recipient_hints.rs.
    // MeshService.kt made the same move, so both shells share one window and
    // one implementation.

    // MARK: - Relay

    private func startRelayLoop() {
        // Push health is unknown at this point (relayPushClient hasn't been
        // (re)started for this session yet) -- start at the
        // unhealthy/background cadence; the first real health report
        // reschedules from there via onRelayPushHealthChanged.
        lastKnownPushHealthy = nil
        relayTimer?.invalidate()
        relayTimer = Timer.scheduledTimer(
            withTimeInterval: TimeInterval(RelayPollPolicy.unhealthyOrBackgroundMs) / 1_000,
            repeats: false
        ) { [weak self] _ in
            Task { @MainActor in self?.relayPollTick() }
        }
        pathMonitor = NWPathMonitor()
        pathMonitor?.pathUpdateHandler = { [weak self] path in
            if path.status == .satisfied {
                Task { @MainActor in self?.runRelaySync() }
            }
            // Recheck the push subscription on every path change, mirroring
            // Android's relayNetworkCallback calling updateRelayPushSubscription
            // from both onCapabilitiesChanged and onLost -- the push socket
            // should be up in exactly the situations runRelaySync would
            // already succeed in, and torn down the moment that stops being
            // true.
            Task { @MainActor in self?.updateRelayPushSubscription() }
        }
        pathMonitor?.start(queue: .global(qos: .utility))

        // Immediate kick on send
        relayCancellable = RelaySyncEvents.subject.sink { [weak self] in
            Task { @MainActor in self?.runRelaySync() }
        }
        updateRelayPushSubscription()
    }

    /// `relayTimer`'s tick: runs the authoritative poll, then reschedules
    /// itself at whatever interval `RelayPollPolicy` currently calls for.
    /// Battery, 2026-07-21: replaces the old fixed-60s repeating timer with a
    /// self-rescheduling one-shot timer (mirrors Android's
    /// `relayPollRunnable` `Runnable.postDelayed` self-repost) so the cadence
    /// can change on every tick. The poll call itself (`runRelaySync`) is
    /// unchanged and stays correctness-authoritative; only its cadence
    /// changes.
    private func relayPollTick() {
        runRelaySync()
        reschedulePoll(currentlyHealthy: relayPushClient.isHealthy())
    }

    /// Recomputes the relay-poll interval from
    /// `RelayPollPolicy.relayPollIntervalMs` given `currentlyHealthy` and the
    /// current `appForeground` state, and re-arms `relayTimer` with it,
    /// cancelling whatever was previously scheduled. Called from
    /// `relayPollTick` itself (every tick decides its own next interval),
    /// from `onRelayPushHealthChanged` (so a health transition reschedules
    /// immediately rather than waiting out whatever long interval is already
    /// pending), and from `setAppForeground` (so a foreground/background flip
    /// reschedules immediately too). Mirrors Android's
    /// `MeshService.reschedulePoll`.
    private func reschedulePoll(currentlyHealthy: Bool) {
        let interval = RelayPollPolicy.relayPollIntervalMs(
            previouslyHealthy: lastKnownPushHealthy,
            currentlyHealthy: currentlyHealthy,
            foreground: appForeground
        )
        lastKnownPushHealthy = currentlyHealthy
        relayTimer?.invalidate()
        relayTimer = Timer.scheduledTimer(
            withTimeInterval: TimeInterval(interval) / 1_000,
            repeats: false
        ) { [weak self] _ in
            Task { @MainActor in self?.relayPollTick() }
        }
    }

    /// `relayPushClient`'s health-change callback -- see `relayPushClient`'s
    /// doc and `RelayPollPolicy`'s type doc. Also mirrors the signal into
    /// `MeshConnectivityStatus.pushHealthy` for `level(for:)`'s relay-health
    /// freshness check: without this, "Online via relay" would falsely
    /// degrade after ~120-150s of push-healthy-but-quiet, since the poll
    /// (which used to be the only thing refreshing `RelayHealth.ok`'s
    /// `lastSyncMs`) now backs off to 900s while foregrounded with push up.
    private func onRelayPushHealthChanged(_ healthy: Bool) {
        log.info("Relay push health -> \(healthy)")
        reschedulePoll(currentlyHealthy: healthy)
        MeshConnectivityStatus.shared.setPushHealthy(healthy)
    }

    /// Starts `relayPushClient` against the user's relay config once the mesh
    /// is running, an identity and relay config exist, and the current
    /// network path is satisfied -- or stops it otherwise. Called from
    /// `startRelayLoop` and on every path update, mirroring the points
    /// `runRelaySync` is itself kicked from (Android
    /// `updateRelayPushSubscription` parity, DTN_TODOS.md D3).
    ///
    /// The hint set passed to `RelayPushClient.start` is recomputed on every
    /// (re)connect via `relayPushHints`, so a contact or group added after
    /// the socket is already open is picked up the next reconnect without
    /// this needing its own change tracking; until then the 60s poll already
    /// covers it.
    private func updateRelayPushSubscription() {
        guard isRunning,
              let identity,
              let config = RelayConfigStore.load(),
              pathMonitor?.currentPath.status == .satisfied
        else {
            relayPushClient.stop()
            return
        }
        let ownUserId = identity.userId
        let subscribeConfig = config
        relayPushClient.start(config: config) {
            relayPushSubscription(ownUserId: ownUserId, config: subscribeConfig)
        }
    }

    private func runRelaySync() {
        guard isRunning, let identity else { return }
        // No own config is no longer a hard stop: contacts' friend cards can
        // carry relays worth polling (Android parity). relaySyncBlocking
        // reports .noConfig when there is truly nowhere to sync.
        let config = RelayConfigStore.load()
        guard pathMonitor?.currentPath.status == .satisfied else {
            MeshConnectivityStatus.shared.setRelayHealth(.noInternet)
            return
        }
        // CP2b: honor relayd's Retry-After. Nudges inside the advertised
        // window (poll tick, push frame, queue change) are dropped; the 60 s
        // poll tick retries once the window has passed. Mirrors
        // RelaySyncEngine.kt's coalesced backoff.
        if Int64(Date().timeIntervalSince1970 * 1_000) < relayRateLimitedUntilMs {
            return
        }
        if relaySyncInFlight {
            relaySyncPending = true
            return
        }
        relaySyncInFlight = true
        backfillRelayReceipts(identity: identity)
        // T23: if our own endpoint changed since the last announcement, queue
        // the notice to every contact *before* this pass uploads, so it rides
        // out in the same sync. This is the single trigger for every way the
        // config can change (Cruise Pass setup and removal, manual entry in
        // Advanced, a scanned setup card, a backup restore) because they all
        // already end in `RelaySyncEvents.requestSync()` — no save site has to
        // remember to announce, and none can be missed. Mirrors Android's
        // `RelaySyncEngine.performRelaySyncPass`.
        RelayUpdateSender.announceIfChanged(store: store, identity: identity)
        MeshRuntimeStatus.shared.markSyncingViaRelay()
        Task.detached(priority: .utility) { [weak self] in
            guard let self else { return }
            await self.relaySyncBlocking(identity: identity, config: config)
            await self.finishRelaySync()
        }
    }

    private func finishRelaySync() {
        relaySyncInFlight = false
        if relaySyncPending, isRunning {
            relaySyncPending = false
            runRelaySync()
        } else {
            relaySyncPending = false
            refreshNearby()
        }
    }

    private func backfillRelayReceipts(identity: Identity) {
        // Core refreshes every contact's DELIVERED/READ receipt envelopes for
        // the current watermarks (was an inline loop here and in
        // MeshService.kt); the returned msg_ids seed the in-memory seen-set
        // for the same reason queueOutgoingReceiptForRelay records there --
        // our own receipt envelope coming back off the relay must dedupe,
        // not get re-carried as foreign mail.
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let msgIds = (try? store.backfillOutgoingReceiptEnvelopes(identity: identity, nowMs: now)) ?? []
        for msgId in msgIds {
            GossipState.seenIds.record(msgId: msgId)
        }
    }

    /// Core owns the mailbox-routing policy (T11): an envelope addressed to a
    /// contact goes to THAT contact's relay mailbox (from their friend card),
    /// falling back to our own saved config. relayd scopes rows per family
    /// token, so posting a cross-family friend request to our own mailbox
    /// strands it where the recipient can never fetch it. Mirrors
    /// RelaySyncEngine.kt's resolvedRelayConfig.
    ///
    /// `endpointUsable: false` means their card endpoint has authoritatively
    /// rejected us and has been written off (core `contact_relay_health`), so
    /// resolution skips it exactly as though the card carried no relay
    /// fields — otherwise one dead field beats a working alternative forever.
    /// Mirrors RelaySyncEngine.kt's resolvedRelayConfig.
    nonisolated static func resolvedRelayConfig(
        contact: Contact?,
        fallback: RelayConfig?,
        endpointUsable: Bool = true
    ) -> RelayConfig? {
        resolvedContactDeliveryRelay(
            contactRelayUrl: contact?.relayUrl,
            contactRelayToken: contact?.relayToken,
            fallbackUrl: fallback?.relayUrl,
            fallbackToken: fallback?.relayToken,
            contactEndpointUsable: endpointUsable
        ).map { RelayConfig(relayUrl: $0.url, relayToken: $0.token) }
    }

    /// CP4: fetch/ack/presence resolution. Post-CP4 friend cards carry
    /// post-only deposit tokens, which cannot read a mailbox — the core
    /// resolves a same-family card back to our own member config and drops
    /// cross-family deposit endpoints entirely (polling them would just 403
    /// `deposit_only` on every pass). Legacy member-token cards keep
    /// proxy-polling exactly as before. Sends stay on `resolvedRelayConfig`.
    /// Mirrors RelaySyncEngine.kt's resolvedPollRelayConfig.
    nonisolated static func resolvedPollRelayConfig(
        contact: Contact?,
        fallback: RelayConfig?,
        endpointUsable: Bool = true
    ) -> RelayConfig? {
        resolvedContactDeliveryPollRelay(
            contactRelayUrl: contact?.relayUrl,
            contactRelayToken: contact?.relayToken,
            fallbackUrl: fallback?.relayUrl,
            fallbackToken: fallback?.relayToken,
            contactEndpointUsable: endpointUsable
        ).map { RelayConfig(relayUrl: $0.url, relayToken: $0.token) }
    }

    /// Whether a contact's card endpoint has earned another attempt, given
    /// the rejection streaks read once at the start of this pass. Mirrors
    /// RelaySyncEngine.kt's contactEndpointUsable.
    nonisolated static func contactEndpointUsable(
        contact: Contact,
        rejections: [Data: ContactRelayRejection],
        nowMs: Int64
    ) -> Bool {
        guard let rejection = rejections[contact.userId] else { return true }
        return coreContactRelayEndpointUsable(
            rejectStreak: rejection.rejectStreak,
            rejectedAtMs: rejection.rejectedAtMs,
            nowMs: nowMs
        )
    }

    /// Every distinct mailbox this device should poll: its own saved config
    /// first, then each contact's resolved card relay (mirrors
    /// RelaySyncEngine.kt's distinctRelayConfigs).
    nonisolated static func distinctRelayConfigs(
        contacts: [Contact],
        fallback: RelayConfig?,
        rejections: [Data: ContactRelayRejection] = [:],
        nowMs: Int64 = 0
    ) -> [RelayConfig] {
        var result: [RelayConfig] = []
        func add(_ cfg: RelayConfig?) {
            guard let cfg else { return }
            if !result.contains(where: { $0.relayUrl == cfg.relayUrl && $0.relayToken == cfg.relayToken }) {
                result.append(cfg)
            }
        }
        add(fallback)
        for contact in contacts {
            add(Self.resolvedPollRelayConfig(
                contact: contact,
                fallback: fallback,
                endpointUsable: Self.contactEndpointUsable(
                    contact: contact,
                    rejections: rejections,
                    nowMs: nowMs
                )
            ))
        }
        return result
    }

    private nonisolated func relaySyncBlocking(identity: Identity, config: RelayConfig?) async {
        let store = AppStore.get()
        // T11 + CP2b: structured rejections of our OWN saved config -- a
        // contact's stale card relay failing is not our pass's fault. The
        // classification (HTTP status/`code` -> semantic fault, transient vs
        // persistent) lives in the core (`core/src/relay_status.rs`); an
        // unstructured failure (.outage) is not recorded because the pass's
        // success flags already express it as .failing. Mirrors
        // RelaySyncEngine.kt's noteOwnRelayFault.
        var ownRelayFault: CoreRelayFault?
        var ownRetryAfterMs: UInt64 = 0
        func noteFailure(_ error: Error, usedConfig: RelayConfig) {
            guard let own = config,
                  usedConfig.relayUrl == own.relayUrl,
                  usedConfig.relayToken == own.relayToken else { return }
            guard let relay = error as? RelayHTTPError else { return }
            let fault = relayClassifyHttpError(
                httpStatus: UInt16(clamping: relay.statusCode),
                relayCode: relay.relayCode
            )
            guard fault != .outage else { return }
            ownRelayFault = RelayHealth.worseFault(ownRelayFault, fault)
            if fault == .rateLimited {
                ownRetryAfterMs = max(ownRetryAfterMs, relayRetryAfterMs(retryAfterHeader: relay.retryAfter))
            }
        }
        do {
            // Upload receipts first, then authored, then family carry --
            // each to the recipient's resolved mailbox, and each post in its
            // own catch so one dead contact relay can't stall the pass
            // (mirrors RelaySyncEngine.kt).
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            _ = try store.pruneExpiredOutgoingReceiptEnvelopes(nowMs: now)
            _ = try store.pruneExpiredOutboundEnvelopes(nowMs: now)
            _ = try store.pruneExpiredCarried(nowMs: now)
            // Same expiry-driven family: once an envelope is expired its relay
            // copy is ackable on the `.expired` disposition alone, so the
            // record that this device consumed it has nothing left to prove.
            _ = try store.pruneExpiredConsumedHiddenMsgIds(nowMs: now)
            let contacts = try store.listContacts()
            let contactsById = Dictionary(
                uniqueKeysWithValues: contacts.map { ($0.userId, $0) }
            )
            // The other half of noteFailure, which had no owner before this:
            // rejections from the endpoint in a CONTACT's friend card. Those
            // were dropped entirely, so a card pointing at a retired host
            // produced an unbounded silent retry loop while the person's
            // messages sat at one tick. Read once per pass; only contacts
            // with a non-zero streak appear. Mirrors RelaySyncEngine.kt.
            var rejections = Dictionary(
                uniqueKeysWithValues: (try store.listContactRelayRejections()).map { ($0.userId, $0) }
            )
            func endpointUsable(_ contact: Contact) -> Bool {
                Self.contactEndpointUsable(contact: contact, rejections: rejections, nowMs: now)
            }
            /// Contacts whose streak already advanced during this pass.
            ///
            /// The core's threshold is worded in *passes* ("requiring the next
            /// pass to agree" -- `CONTACT_RELAY_STALE_STREAK`), which is what
            /// rules out a relay answering mid-redeploy from a
            /// half-initialised process. Counting per envelope quietly broke
            /// that: a contact with two queued messages was written off inside
            /// a single pass, the exact false positive the second pass exists
            /// to prevent. Mirrors RelaySyncEngine.kt.
            var countedThisPass: Set<Data> = []
            /// Contacts whose endpoint failed this pass without answering at
            /// all. Held to the end of the pass because the observation only
            /// means anything next to proof that this device's internet works.
            var silentThisPass: [Data: String] = [:]
            /// A rest belongs to an *address*, not to a person: `relayCursorKey`
            /// hashes the contact's current endpoint so a card or a T23 notice
            /// that moves them to a different host is tried again immediately
            /// instead of serving out the old host's rest window.
            func endpointKey(_ contact: Contact) -> String {
                relayCursorKey(
                    relayUrl: contact.relayUrl ?? "",
                    relayToken: contact.relayToken ?? ""
                )
            }
            /// Whether this contact's endpoint has answered recently enough to
            /// be worth a request. The counterpart to `endpointUsable` for the
            /// failure mode with no HTTP answer to classify.
            func endpointAnswering(_ contact: Contact) -> Bool {
                ContactRelaySilence.shared.endpointAnswering(
                    userId: contact.userId,
                    endpointKey: endpointKey(contact),
                    nowMs: now
                )
            }
            /// Where a send to this contact should go, or nil for "no relay
            /// attempt right now", which leaves the envelope queued for a
            /// later pass and for the BLE/LAN paths.
            ///
            /// Nil is deliberately the answer for a *silent* endpoint, and it
            /// is not the answer a rejected one gets. A rejection proves the
            /// card is wrong, so falling back to our own relay costs nothing
            /// and delivers outright when both sides have since moved to the
            /// same new host. Silence proves nothing -- the host may be
            /// rebooting -- and falling back would post a cross-family
            /// contact's mail into our own mailbox, which they never read.
            /// `relayPostedAt` is terminal, so that misroute would not be a
            /// retry: the envelope would never be offered to the relay path
            /// again. Mirrors RelaySyncEngine.kt's resolvedRelayConfig.
            func sendConfig(for contact: Contact) -> RelayConfig? {
                guard endpointAnswering(contact) else { return nil }
                return Self.resolvedRelayConfig(
                    contact: contact,
                    fallback: config,
                    endpointUsable: endpointUsable(contact)
                )
            }
            /// Only counts when `usedConfig` is genuinely the contact's own
            /// endpoint: once we have fallen back to our own relay, a failure
            /// there is our relay's health, not evidence about their card.
            func noteContactFailure(_ error: Error, contact: Contact, usedConfig: RelayConfig) {
                if let own = config,
                   usedConfig.relayUrl == own.relayUrl,
                   usedConfig.relayToken == own.relayToken { return }
                guard let relay = error as? RelayHTTPError else {
                    // No HTTP answer at all -- a retired host, dead DNS, a
                    // refused connection. Not evidence about the card on its
                    // own, so it is only remembered here; the end of the pass
                    // decides whether this device had any business believing
                    // it.
                    silentThisPass[contact.userId] = endpointKey(contact)
                    return
                }
                let fault = relayClassifyHttpError(
                    httpStatus: UInt16(clamping: relay.statusCode),
                    relayCode: relay.relayCode
                )
                guard coreContactRelayStreakDelta(fault: fault) != 0 else { return }
                guard countedThisPass.insert(contact.userId).inserted else { return }
                if let streak = try? store.noteContactRelayRejected(userId: contact.userId, nowMs: now) {
                    rejections[contact.userId] = ContactRelayRejection(
                        userId: contact.userId,
                        rejectStreak: streak,
                        rejectedAtMs: now
                    )
                }
            }
            /// Success is the only thing that clears a streak -- see
            /// `clear_contact_relay_rejection` for why a transient fault
            /// deliberately does not.
            func noteContactSuccess(contact: Contact, usedConfig: RelayConfig) {
                if let own = config,
                   usedConfig.relayUrl == own.relayUrl,
                   usedConfig.relayToken == own.relayToken { return }
                // The endpoint answering settles the silence question outright,
                // whatever this pass had provisionally observed.
                silentThisPass[contact.userId] = nil
                ContactRelaySilence.shared.noteAnswered(userId: contact.userId)
                guard rejections[contact.userId] != nil else { return }
                try? store.clearContactRelayRejection(userId: contact.userId)
                rejections[contact.userId] = nil
                countedThisPass.remove(contact.userId)
            }
            let receipts = try store.pendingRelayOutgoingReceiptEnvelopes(
                limit: MeshDefaults.relayStoreBatchLimit,
                nowMs: now
            )
            for env in receipts {
                guard let contact = contactsById[env.recipientUserId],
                      let cfg = sendConfig(for: contact)
                else { continue }
                do {
                    _ = try RelayClient.postReceiptEnvelope(config: cfg, envelope: env)
                    _ = try store.markOutgoingReceiptEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    noteContactSuccess(contact: contact, usedConfig: cfg)
                } catch {
                    noteFailure(error, usedConfig: cfg)
                    noteContactFailure(error, contact: contact, usedConfig: cfg)
                }
            }
            let outbound = try store.pendingRelayOutboundEnvelopes(
                limit: MeshDefaults.relayStoreBatchLimit,
                nowMs: now
            )
            let importedGroups = try store.listGroups()
            let groupsById = Dictionary(
                uniqueKeysWithValues: importedGroups.map { ($0.id, $0) }
            )
            /// Which single mailbox a group envelope's fan-out rows go to, or
            /// nil for "post nothing this pass" -- which leaves the envelope
            /// queued for a later pass and for the BLE/LAN paths, exactly as
            /// the 1:1 skip does.
            ///
            /// The choice itself is core's because the rule that matters is
            /// easy to get subtly wrong in one shell: a member whose endpoint
            /// is *resting for silence* contributes no fallback to our own
            /// mailbox. Falling back for them would post a cross-family
            /// member's copy where they never read, and `relayPostedAt` is
            /// terminal, so that is a permanent misroute rather than a retry.
            /// A member written off for *rejection* still falls back,
            /// unchanged. Mirrors RelaySyncEngine.kt.
            func relayConfigForGroupRecipient(_ groupId: Data) -> RelayConfig? {
                guard let group = groupsById[groupId] else { return config }
                let members = group.memberUserIds.compactMap { member -> GroupRelayMember? in
                    guard let contact = contactsById[member] else { return nil }
                    return GroupRelayMember(
                        relayUrl: contact.relayUrl,
                        relayToken: contact.relayToken,
                        endpointUsable: endpointUsable(contact),
                        endpointAnswering: endpointAnswering(contact)
                    )
                }
                guard let target = coreGroupFanoutRelayTarget(
                    members: members,
                    fallbackUrl: config?.relayUrl,
                    fallbackToken: config?.relayToken
                ) else { return nil }
                return RelayConfig(relayUrl: target.url, relayToken: target.token)
            }
            for env in outbound {
                guard let contact = contactsById[env.recipientUserId] else {
                    guard let cfg = relayConfigForGroupRecipient(env.recipientUserId) else { continue }
                    guard let group = groupsById[env.recipientUserId] else {
                        // Recipient is neither contact nor imported group
                        // (e.g. a group deleted mid-queue); keep the legacy
                        // single post so the envelope isn't stranded.
                        do {
                            _ = try RelayClient.postOutboundEnvelope(config: cfg, envelope: env)
                            _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                        } catch { noteFailure(error, usedConfig: cfg) }
                        continue
                    }
                    // Group-addressed: per-member fan-out instead of one
                    // shared group-hint row (specs/group-relay-durability.md
                    // §4.2). Mark relay-posted only after ALL member rows
                    // post; a partial failure retries the whole set next
                    // pass, and the deterministic fan-out msg_ids dedupe
                    // server-side. Mirrors RelaySyncEngine.kt.
                    let rows = coreGroupFanoutRows(
                        originalMsgId: env.msgId,
                        memberUserIds: group.memberUserIds,
                        hopTtl: env.hopTtl,
                        expiry: env.expiry,
                        sealed: env.sealed,
                        envelopeTimestampMs: env.timestamp
                    )
                    var posted = 0
                    for row in rows {
                        do {
                            _ = try RelayClient.postFanoutRow(config: cfg, row: row)
                            posted += 1
                        } catch { noteFailure(error, usedConfig: cfg) }
                    }
                    if posted == rows.count {
                        _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    }
                    continue
                }
                guard let cfg = sendConfig(for: contact) else { continue }
                do {
                    _ = try RelayClient.postOutboundEnvelope(config: cfg, envelope: env)
                    _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    noteContactSuccess(contact: contact, usedConfig: cfg)
                } catch {
                    noteFailure(error, usedConfig: cfg)
                    noteContactFailure(error, contact: contact, usedConfig: cfg)
                }
            }
            let family = try store.familyCarriedEnvelopes(
                limit: MeshDefaults.relayStoreBatchLimit,
                nowMs: now
            )
            for env in family {
                // Carried mail goes to the mailbox its recipient actually
                // polls: a contact hint posts to that contact's resolved
                // relay; a group hint decomposes into per-member fan-out rows
                // (specs/group-relay-durability.md §4.2); unrecognizable
                // hints are skipped. A successful post stamps the row's
                // upload marker so the next pass offers the NEXT batch
                // instead of re-posting this one for its whole seven-day
                // life (see markCarriedEnvelopeRelayUploaded in core).
                // Mirrors RelaySyncEngine.kt.
                if let contact = (try? store.contactMatchingHint(hint: env.recipientHint, nowMs: now)) ?? nil {
                    guard let cfg = sendConfig(for: contact) else { continue }
                    do {
                        _ = try RelayClient.postCarriedEnvelope(config: cfg, envelope: env)
                        _ = try store.markCarriedEnvelopeRelayUploaded(msgId: env.msgId, relayUrl: cfg.relayUrl)
                        noteContactSuccess(contact: contact, usedConfig: cfg)
                    } catch {
                        noteFailure(error, usedConfig: cfg)
                        noteContactFailure(error, contact: contact, usedConfig: cfg)
                    }
                    continue
                }
                if let group = (try? store.groupMatchingHint(hint: env.recipientHint, nowMs: now)) ?? nil {
                    guard let cfg = relayConfigForGroupRecipient(group.id) else { continue }
                    let rows = coreGroupFanoutRowsForCarried(
                        originalMsgId: env.msgId,
                        memberUserIds: group.memberUserIds,
                        hopTtl: env.hopTtl,
                        expiry: env.expiry,
                        sealed: env.sealed
                    )
                    // Stamped only once EVERY fan-out row landed -- a partial
                    // batch re-posts whole next pass, and the deterministic
                    // fan-out ids dedupe the rows that did land.
                    var posted = 0
                    for row in rows {
                        do {
                            _ = try RelayClient.postFanoutRow(config: cfg, row: row)
                            posted += 1
                        } catch { noteFailure(error, usedConfig: cfg) }
                    }
                    if posted == rows.count, !rows.isEmpty {
                        _ = try? store.markCarriedEnvelopeRelayUploaded(msgId: env.msgId, relayUrl: cfg.relayUrl)
                    }
                    continue
                }
            }

            // Gaining a contact or a group widens the fetch-hint set, and
            // relayd's next_cursor only ever covers the hints we sent -- so
            // mail that arrived under a hint we did not have yet is already
            // *below* the frontier, where no sweep interval can reach it. Core
            // notices the change and drops the frontiers; the walks below then
            // start at 0. Cheap when nothing changed (one digest of the id
            // set, no rows touched), so it is safe to run every pass.
            // Mirrors RelaySyncEngine.kt.
            if (try? store.noteRelayHintSources(ownUserId: identity.userId)) == true {
                relaySyncLog.info(
                    "Hint sources changed; re-walking every relay mailbox from the start"
                )
            }

            // Fetch-side parity with RelaySyncEngine.kt: poll every distinct
            // mailbox we know about -- our own plus each contact's card
            // relay. Mail addressed to us doesn't always reach our own
            // mailbox (a sender may only have had a fallback config, or an
            // older build posted to its own family mailbox), so checking one
            // box quietly loses cross-token mail.
            let distinctConfigs = Self.distinctRelayConfigs(
                // Reading a mailbox that is not answering is the same waste as
                // posting to it, and there is no fallback on the poll path
                // either way, so a rested endpoint simply drops out of the set.
                contacts: contacts.filter { endpointAnswering($0) },
                fallback: config,
                rejections: rejections,
                nowMs: now
            )
            guard !distinctConfigs.isEmpty else {
                await MainActor.run {
                    MeshConnectivityStatus.shared.setRelayHealth(.noConfig)
                }
                return
            }
            let presenceHints: (Data, Int64) -> [Data] = { userId, timestamp in
                recentPresenceHintsFor(userId: userId, nowMs: timestamp)
            }
            let fetchHints = try store.relayFetchHints(ownUserId: identity.userId, nowMs: now)
            var anyRelaySucceeded = false
            var ownRelaySucceeded = config == nil
            // Distinct from ownRelaySucceeded, which starts true when there is
            // no own relay at all. Only a mailbox that actually answered
            // counts as proof this device has working internet, and only that
            // may license resting a contact's silent endpoint.
            var ownRelayAnswered = false
            for cfg in distinctConfigs {
                // Presence rides each mailbox for the contacts resolved to
                // it; own presence is announced everywhere so contacts on
                // other relays still see us (mirrors syncRelayPresence in
                // RelaySyncEngine.kt). Presence failure is never fatal.
                // CP4: presence is a read, so contacts group under their
                // *poll* config — a family member's deposit-token card
                // resolves back to our own member config and their presence
                // keeps flowing through it.
                let contactsForConfig = contacts.filter { contact in
                    guard let resolved = Self.resolvedPollRelayConfig(contact: contact, fallback: config)
                    else { return false }
                    return resolved.relayUrl == cfg.relayUrl && resolved.relayToken == cfg.relayToken
                }
                if !contactsForConfig.isEmpty {
                    let announce = RelayConfigStore.shareOnline()
                        ? presenceHints(identity.userId, now)
                        : []
                    let query = Array(Set(contactsForConfig.flatMap { presenceHints($0.userId, now) }))
                    if !announce.isEmpty || !query.isEmpty {
                        let contactByHint = Dictionary(uniqueKeysWithValues: contactsForConfig.flatMap { contact in
                            presenceHints(contact.userId, now).map { ($0, contact.userId) }
                        })
                        do {
                            let page = try RelayClient.syncPresence(
                                config: cfg,
                                announce: announce,
                                query: query
                            )
                            let localNow = Int64(Date().timeIntervalSince1970 * 1_000)
                            await MainActor.run {
                                for item in page.presence {
                                    guard let userId = contactByHint[item.hint] else { continue }
                                    let localSeenAt = localNow - max(0, page.nowMs - item.lastSeenMs)
                                    MeshConnectivityStatus.shared.mergePresenceLastSeen(
                                        userId: userId,
                                        seenAtMs: localSeenAt
                                    )
                                    try? store.recordPeerConnectionEvent(
                                        userId: userId,
                                        transport: .cruisePass,
                                        kind: .presenceSeen,
                                        occurredAtMs: localSeenAt
                                    )
                                }
                            }
                        } catch { noteFailure(error, usedConfig: cfg) }
                    }
                }
                // FI2 proxy-polling included: core's relayFetchHints is self +
                // member-group + every contact's recent-day hints, deduped
                // (core/src/recipient_hints.rs). Proxy-fetched mail this device
                // can't decrypt falls into the carry-foreign path and comes back
                // `.carried`, never `.consumed`, so coreRelayAckIdsWithConsumed
                // keeps the DTN ack invariant exactly as before: a carried relay
                // copy is never acked away.
                //
                // Where the walk starts (the persistent frontier). This used
                // to begin every pass at `afterId = 0` and page to the end.
                // The un-acked rows above are left on the relay by design, so
                // a real mailbox only grows, relayd returns rows in ascending
                // id order, and a *fresh* message therefore has the highest id
                // and was fetched last -- after every stale row ahead of it.
                // In the field that reached ~29k rows at 16 rows a page:
                // thousands of sequential round trips before the newest
                // message was looked at, and passes that died on a timeout
                // before finishing. Messages took minutes to arrive.
                //
                // A pass now resumes from the frontier persisted for this
                // mailbox and advances it through `advanceRelayFetchCursor`,
                // which never moves past a page that failed to fully process
                // or to land its acks, and never moves backwards -- the mirror
                // of the DTN ack-safety rule applied to skipping. Occasionally
                // it sweeps the whole mailbox from 0 instead, so the rows that
                // are supposed to stay there remain re-discoverable and a
                // rebuilt relay heals itself. `relaySweepDue` owns when, from
                // the persisted sweep timestamp: every `relaySweepIntervalMs`,
                // plus the first pass against a mailbox never swept at all --
                // notably NOT every process start, which would tie a full
                // re-download of the mailbox to the restart rate. Mirrors
                // RelaySyncEngine.kt.
                let cursorKey = relayCursorKey(relayUrl: cfg.relayUrl, relayToken: cfg.relayToken)
                do {
                    let cursor = try store.relayFetchCursor(configKey: cursorKey)
                    let sweeping = relaySweepDue(
                        sweptThisSession: RelaySweepSession.shared.hasSwept(cursorKey),
                        lastSweepAtMs: cursor.lastSweepAtMs,
                        nowMs: now
                    )
                    var afterId = relayPassStartCursor(
                        sweeping: sweeping,
                        persistedAfterId: cursor.afterId
                    )
                    // Once a page fails to fully process, the frontier stops
                    // moving for the rest of this pass: persisting a later
                    // page's cursor would skip the failed one forever. The
                    // walk itself continues, so one bad page never blocks the
                    // mail behind it.
                    var frontierAdvancing = true
                    // Not a `let`: a page this client cannot take -- too big to
                    // decode, or too big to finish moving over this link --
                    // halves the ask and retries the same cursor, and the
                    // reduced limit is kept for the rest of this mailbox's
                    // walk rather than reset per page -- a mailbox that
                    // produced one oversize window usually produces the next
                    // one too, and rediscovering that costs a wasted request
                    // every page. Scoped to THIS mailbox, exactly as in
                    // RelaySyncEngine.kt, where it is a local of
                    // `pollRelayMailbox`: one relay's oversize page says
                    // nothing about the next relay's, and carrying the
                    // reduction across configs would shrink every other
                    // mailbox's pages for the rest of the pass. The next pass
                    // starts from the full limit again.
                    var fetchBatchLimit = Int(relayFetchBatchLimit())
                    // Set the moment a page comes back: the caller uses this
                    // as proof this device's internet works, so it must mean
                    // "this mailbox answered", not "the walk was attempted".
                    var mailboxAnswered = false
                    func finishSweep() {
                        guard sweeping else { return }
                        RelaySweepSession.shared.noteSwept(cursorKey)
                        try? store.noteRelaySweepCompleted(configKey: cursorKey, nowMs: now)
                    }
                    while true {
                        let fetched = try RelayClient.fetchEnvelopesWithinResponseCap(
                            config: cfg,
                            hints: fetchHints,
                            afterId: afterId,
                            limit: fetchBatchLimit
                        ) { tried, smaller in
                            relaySyncLog.warning(
                                "Relay page was too big to take at limit=\(tried, privacy: .public); retrying with limit=\(smaller, privacy: .public)"
                            )
                        }
                        let page = fetched.page
                        // Carried to the next page of THIS mailbox only; see
                        // the declaration above for why.
                        fetchBatchLimit = fetched.limit
                        mailboxAnswered = true
                        guard !page.envelopes.isEmpty else {
                            finishSweep()
                            break
                        }
                        var pageFullyProcessed = true
                        var dispositions: [CoreRelayEnvelopeDisposition] = []
                        for env in page.envelopes {
                            let disposition = await MainActor.run {
                                MeshController.shared.processInboundEnvelope(
                                    sourceAddress: nil,
                                    msgId: env.msgId,
                                    hopTtl: env.hopTtl,
                                    expiry: env.expiryMs,
                                    recipientHint: env.recipientHint,
                                    sealed: env.sealed,
                                    identity: identity
                                )
                            }
                            dispositions.append(CoreRelayEnvelopeDisposition(
                                relayId: env.id,
                                msgId: env.msgId,
                                disposition: disposition,
                                recipientHint: env.recipientHint
                            ))
                            // A contact-hinted envelope coming out of THIS
                            // mailbox is proof the mailbox its recipient
                            // polls already holds it (proxy-poll parity: a
                            // contact's hints are only ever fetched against
                            // that contact's resolved relay). If we also
                            // carry the same msg_id from a BLE/LAN encounter,
                            // stamp that row so the upload loop stops
                            // re-posting a copy the relay demonstrably has
                            // (no-op when we carry nothing). Group-hinted
                            // rows are deliberately NOT stamped here -- they
                            // are stamped only by a complete fan-out post.
                            // Bookkeeping only, so a failure must not fail
                            // the walk. Mirrors RelaySyncEngine.kt.
                            if let _ = (try? store.contactMatchingHint(hint: env.recipientHint, nowMs: now)) ?? nil {
                                _ = try? store.markCarriedEnvelopeRelayUploaded(
                                    msgId: env.msgId,
                                    relayUrl: cfg.relayUrl
                                )
                            }
                        }
                        // Consumed/Expired ack unconditionally; a SEEN envelope is
                        // acked only if this device durably consumed it as a 1:1
                        // message from someone else (DTN_TODOS.md §3.1); a legacy
                        // shared-mailbox group-hint row is never acked at all
                        // (specs/group-relay-durability.md §5.2) -- see
                        // CoreRelayEnvelopeDisposition's doc comment in engine.rs.
                        do {
                            let acks = try store.coreRelayAckIdsWithConsumed(
                                items: dispositions,
                                ownUserId: identity.userId,
                                nowMs: now
                            )
                            // An ack that never landed leaves consumed rows in
                            // the mailbox; skipping past them would strand them
                            // there until expiry, so the frontier waits for the
                            // next pass to retry.
                            if !acks.isEmpty {
                                try RelayClient.ackEnvelopes(config: cfg, ids: acks)
                            }
                        } catch {
                            pageFullyProcessed = false
                            noteFailure(error, usedConfig: cfg)
                            relaySyncLog.warning(
                                "Relay page ack failed: \(error.localizedDescription, privacy: .public)"
                            )
                        }
                        if !pageFullyProcessed { frontierAdvancing = false }
                        if frontierAdvancing {
                            _ = try? store.advanceRelayFetchCursor(
                                configKey: cursorKey,
                                pageNextCursor: page.nextCursor,
                                pageFullyProcessed: true
                            )
                        }
                        // End the walk on an EMPTY page, never on a short one:
                        // a server may clamp `limit=` below our ask, and
                        // reading a short page as end-of-mailbox would strand
                        // every row above it -- in an ascending-id mailbox,
                        // all the new mail. Reaching here with a non-empty
                        // page means the cursor stood still, which relayd
                        // cannot produce -- a bail-out, not end-of-mailbox, so
                        // it deliberately does not record a completed sweep.
                        guard relayFetchWalkContinues(
                            pageEnvelopeCount: UInt32(clamping: page.envelopes.count),
                            afterId: afterId,
                            pageNextCursor: page.nextCursor
                        ) else {
                            relaySyncLog.warning(
                                "Relay returned rows without advancing the cursor; ending the walk"
                            )
                            break
                        }
                        afterId = page.nextCursor
                    }
                    anyRelaySucceeded = true
                    if let own = config, cfg.relayUrl == own.relayUrl, cfg.relayToken == own.relayToken {
                        ownRelaySucceeded = true
                        if mailboxAnswered { ownRelayAnswered = true }
                    }
                } catch {
                    // A contact can carry stale relay credentials from an
                    // older friend card. That mailbox failing must not abort
                    // polling of the remaining relays or declare our own
                    // configured relay unreachable when it succeeded.
                    noteFailure(error, usedConfig: cfg)
                }
            }
            // Now that the pass knows whether our own mailbox answered, this
            // pass's silent contact endpoints can be judged -- or discarded.
            // `ownRelayAnswered` is handed straight to the core rather than
            // tested here: whether same-pass proof of working internet is
            // required, and what its absence means, is one rule both shells
            // must answer identically, so coreContactRelayUnreachableDelta is
            // the only place it is decided. Without the proof nothing is
            // recorded -- a phone in a tunnel fails every endpoint at once,
            // and resting them all would take the relay path away from every
            // contact for the whole rest window. Mirrors RelaySyncEngine.kt's
            // commitUnreachableContactRelays.
            for (userId, key) in silentThisPass {
                guard let streak = ContactRelaySilence.shared.noteSilentPass(
                    userId: userId,
                    endpointKey: key,
                    otherRelayAnswered: ownRelayAnswered,
                    nowMs: now
                ) else { continue }
                relaySyncLog.warning(
                    "A contact's relay endpoint did not answer while our own did (silent passes=\(streak, privacy: .public)); resting it rather than retrying every pass"
                )
            }
            silentThisPass.removeAll()
            let syncedAtMs = Int64(Date().timeIntervalSince1970 * 1_000)
            let fault = ownRelayFault
            let retryAfterMs = ownRetryAfterMs
            let ownSucceeded = ownRelaySucceeded
            let anySucceeded = anyRelaySucceeded
            // Reported from the streak alone, not from `endpointUsable`: a
            // card stays reported stale through its six-hourly probe window,
            // so the explanation in the contact sheet doesn't blink out and
            // back while nothing about the person's situation changed.
            let stale = Set(
                rejections.values
                    .filter { coreContactRelayIsStale(rejectStreak: $0.rejectStreak) }
                    .map(\.userId)
            )
            await MainActor.run {
                MeshConnectivityStatus.shared.setRelayHealth(RelayHealth.afterSyncPass(
                    fault: fault,
                    ownRelaySucceeded: ownSucceeded,
                    anyRelaySucceeded: anySucceeded,
                    nowMs: syncedAtMs
                ))
                MeshConnectivityStatus.shared.setStaleRelayContacts(stale)
                self.noteRelayRateLimit(fault: fault, retryAfterMs: retryAfterMs)
            }
        } catch {
            if let config { noteFailure(error, usedConfig: config) }
            let message = error.localizedDescription
            let fault = ownRelayFault
            let retryAfterMs = ownRetryAfterMs
            await MainActor.run {
                let nowMs = Int64(Date().timeIntervalSince1970 * 1_000)
                MeshConnectivityStatus.shared.setRelayHealth(RelayHealth.afterSyncPass(
                    fault: fault,
                    ownRelaySucceeded: false,
                    anyRelaySucceeded: false,
                    nowMs: nowMs
                ))
                self.noteRelayRateLimit(fault: fault, retryAfterMs: retryAfterMs)
                log.warning("Relay sync failed: \(message, privacy: .public)")
            }
        }
    }

    /// CP2b: remember (or clear) the window relayd's Retry-After asked us to
    /// stay quiet for. `runRelaySync` consults it before starting a pass.
    private func noteRelayRateLimit(fault: CoreRelayFault?, retryAfterMs: UInt64) {
        if fault == .rateLimited {
            relayRateLimitedUntilMs = Int64(Date().timeIntervalSince1970 * 1_000) + Int64(retryAfterMs)
        } else {
            relayRateLimitedUntilMs = 0
        }
    }

    /// Records that a friend's own message landed on this phone, for the
    /// Connection details screen.
    ///
    /// Deliberately narrow. Only kinds a person actually sees in a
    /// conversation count (`isVisibleChatKind`) -- receipts, profile sync,
    /// relay updates, endpoint hints, reactions and every other hidden kind
    /// are machine chatter and would make the screen claim a friend had
    /// written when nobody did. Unknown senders are skipped too: the screen
    /// only lists friends, so an event for anyone else could never be shown
    /// against a name.
    ///
    /// Best-effort: connection history is a diagnostic, never worth failing a
    /// real message delivery over.
    private func recordInboundChatArrival(
        senderUserId: Data,
        kind: UInt8,
        arrival: MessageArrival?
    ) {
        guard isVisibleChatKind(kind), let arrival else { return }
        guard (try? store.getContact(userId: senderUserId)) != nil else { return }
        try? store.recordPeerConnectionEvent(
            userId: senderUserId,
            transport: corePeerTransportForArrival(transport: arrival.transport),
            kind: .messageReceived,
            occurredAtMs: arrival.receivedAt
        )
    }

    private func recordPeerDisconnected(address: String) {
        guard let userId = MeshRouter.userIdFor(address: address),
              let transport = MeshRouter.transportFor(address: address),
              (try? store.getContact(userId: userId)) != nil else { return }
        recordPeerConnection(userId: userId, transport: transport, kind: .disconnected)
    }

    private func recordPeerConnection(
        userId: Data,
        transport: MeshRouterState.Transport,
        kind: PeerConnectionEventKind
    ) {
        let path: PeerConnectionTransport = transport == .lan ? .localWifi : .bluetooth
        try? store.recordPeerConnectionEvent(
            userId: userId,
            transport: path,
            kind: kind,
            occurredAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )
    }

    private func refreshNearby() {
        guard isRunning else { return }
        MeshConnectivityStatus.shared.refreshNearbyRoutes()
        MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
    }
}

/// Self + owned-group recipient hints for the current moment -- the same hint
/// set `MeshController.relaySyncBlocking` computes inline for its own relay
/// fetch. A free function (not a `MeshController` method) so it carries no
/// main-actor isolation: `RelayPushClient` (DTN_TODOS.md D3) invokes its
/// `hintsProvider` closure from its own private queue, off the main actor,
/// the same reason `relaySyncBlocking` itself is `nonisolated` and
/// calls the store (safe off-actor) rather than any `@MainActor`-isolated
/// controller member.
/// FI2: unlike this function, `relaySyncBlocking`'s own fetch ALSO includes
/// `relayProxyHints` below (mail addressed to a contact, fetched on their
/// behalf) -- deliberately not mirrored here. This hint set only decides
/// which relay topics wake the push socket/reconnect; the proxy hint set
/// scales with contact-list size (one subscription per contact per
/// recent day), and the 60s poll already covers proxy-fetched mail without
/// needing a push nudge for it. Revisit if proxy-fetch latency ever needs to
/// beat the poll interval.
///
/// Uses `relaySelfPushHints`, not `relaySelfHints`: the push subscription is
/// computed once per socket connect and the socket then stays open
/// indefinitely (relayd keepalive pings), so a socket opened before the UTC
/// day rollover would otherwise subscribe only to hints that stop matching
/// anything the moment the day-salt rotates. `relaySelfPushHints` adds one
/// day ahead for the same ids so the subscription survives the rollover;
/// see its doc for why that's safe (it only widens what the subscription
/// matches -- envelopes are still ever created with a backward-looking
/// hint).
private func relayPushHints(ownUserId: Data) -> [Data] {
    let store = AppStore.get()
    let now = Int64(Date().timeIntervalSince1970 * 1000)
    // On a store error fall back to just our own hints, matching the old
    // inline loop.
    return (try? store.relaySelfPushHints(ownUserId: ownUserId, nowMs: now))
        ?? recentHintsFor(userId: ownUserId, nowMs: now)
}

/// The full `/ws` subscribe: `relayPushHints` plus the poll path's persisted
/// fetch frontier for this relay, so a reconnect asks relayd to replay from
/// there rather than from 0 (which replayed the entire mailbox as frames the
/// doorbell then discarded). Free function for the same reason
/// `relayPushHints` is one: `RelayPushClient` invokes it off the main actor.
private func relayPushSubscription(ownUserId: Data, config: RelayConfig) -> RelayPushSubscription {
    let store = AppStore.get()
    let cursorKey = relayCursorKey(relayUrl: config.relayUrl, relayToken: config.relayToken)
    let afterId = (try? store.relayFetchCursor(configKey: cursorKey))?.afterId ?? 0
    return RelayPushSubscription(hints: relayPushHints(ownUserId: ownUserId), afterId: afterId)
}

