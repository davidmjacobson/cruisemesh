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
    private var pathMonitor: NWPathMonitor?
    /// DTN_TODOS.md D3 (iOS half of audit finding F1, "relay poll-only"): opens
    /// relayd's `GET /ws` push socket (see `RelayPushClient`'s class doc) once
    /// the mesh is running, an identity/relay config exist, and the network
    /// path is satisfied, and calls `runRelaySync()` on every pushed frame
    /// instead of waiting for the next `relayTimer` tick. Never processes
    /// envelope content itself -- see `RelayPushClient`'s class doc. Mirrors
    /// Android's `relayPushClient` / `updateRelayPushSubscription`
    /// (`MeshService.kt`).
    private lazy var relayPushClient = RelayPushClient { [weak self] in
        Task { @MainActor in self?.runRelaySync() }
    }
    private var isRunning = false
    private var meshRolesRunning = false
    private var pausedForBluetoothAudio = false
    private var relayCancellable: AnyCancellable?
    private var lanHealthTimer: Timer?
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
            if pausedForBluetoothAudio {
                MeshRuntimeStatus.shared.markPausedForBluetoothAudio()
            } else {
                MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
            }
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
        startLanHealthLoop()

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
            MeshRouter.onDisconnected(address: address)
            Task { @MainActor in MeshController.shared.refreshNearby() }
        }
        transport.onPeripheralSubscribed = { [weak self] address in
            Task { @MainActor in
                MeshRouter.onConnected(address: address, transport: .peripheral)
                self?.sendHello(address: address)
                self?.refreshNearby()
            }
        }
        transport.onPeripheralUnsubscribed = { address in
            MeshRouter.onDisconnected(address: address)
            Task { @MainActor in MeshController.shared.refreshNearby() }
        }

        registerBluetoothAudioObserver()
        startRelayLoop()
        refreshBluetoothAudioBackoff(reason: "mesh start")
        log.info("Mesh started")
    }

    func stop() {
        guard isRunning else { return }
        isRunning = false
        pausedForBluetoothAudio = false
        bluetoothAudioBackoff.reset()
        unregisterBluetoothAudioObserver()
        lanTransport?.stop()
        lanTransport = nil
        LanTransportDiagnostics.shared.unregister()
        lanHealthTimer?.invalidate()
        lanHealthTimer = nil
        lanHealth.clear()
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
        appForeground = foreground
        lanTransport?.setForegroundActive(foreground)
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
                self?.refreshBluetoothAudioBackoff(reason: "route change")
            }
        }
    }

    private func unregisterBluetoothAudioObserver() {
        if let audioRouteObserver {
            NotificationCenter.default.removeObserver(audioRouteObserver)
            self.audioRouteObserver = nil
        }
    }

    private func refreshBluetoothAudioBackoff(reason: String) {
        guard isRunning else { return }
        switch bluetoothAudioBackoff.update(bluetoothAudioActive: isBluetoothAudioActive()) {
        case .active:
            pausedForBluetoothAudio = false
            log.info("Bluetooth audio clear; resuming BLE mesh (\(reason, privacy: .public))")
            startMeshRoles()
            MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
        case .pausedForBluetoothAudio:
            pausedForBluetoothAudio = true
            log.info("Bluetooth audio active; pausing BLE mesh to protect audio (\(reason, privacy: .public))")
            stopMeshRoles()
            MeshRuntimeStatus.shared.markPausedForBluetoothAudio()
        case nil:
            break
        }
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
        let through = (try? store.highestContiguousLamport(chatId: chatId, senderUserId: chatId)) ?? 0
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
            let through = (try? store.highestContiguousLamport(
                chatId: groupId,
                senderUserId: senderUserId
            )) ?? 0
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
        LanEndpointCache.save(
            networkId: currentLanNetworkId,
            userId: userId,
            endpoint: endpoint
        )
        queueCurrentLanEndpoint(to: userId)
        guard MeshRouter.transportFor(address: address) != .lan else { return }
        lanTransport?.connect(endpoint, remoteInstanceToken: instanceToken)
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

    private func startLanHealthLoop() {
        lanHealthTimer?.invalidate()
        lanHealthTimer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in
            Task { @MainActor in
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
        log.info("HELLO from \(address, privacy: .public) \(UserIdHex.encode(userId), privacy: .public)")
        sendLanEndpointHint(address: address)
        queueCurrentLanEndpoint(to: userId)
        drainCarriedEnvelopesTo(address: address, peerUserId: userId)
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
        refreshNearby()
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
            let missing = (try? store.messagesAfter(
                chatId: contact.userId,
                senderUserId: identity.userId,
                afterLamport: peerHasThrough
            )) ?? []
            for message in missing {
                let outbound = byLamport[message.lamport]
                    ?? backfillOutbound(identity: identity, contact: contact, message: message)
                if let outbound {
                    MeshRouter.sendToAddress(address: address, frame: encodeOutboundEnvelopeFrame(outbound))
                }
            }
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
            if let (group, opened) = tryOpenGroupMessage(recipientHint: recipientHint, sealed: sealed, now: now) {
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
        do {
            try deliverOpened(
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
        // DTN D4: delivery ran to completion -- safe, and required, to record.
        GossipState.seenIds.record(msgId: msgId)
        return .consumed
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
        let hopsTaken = MeshDefaults.hopTtl >= receivedHopTtl
            ? MeshDefaults.hopTtl - receivedHopTtl
            : 0
        return MessageArrival(
            transport: transport,
            hopsTaken: hopsTaken,
            receivedAt: Int64(Date().timeIntervalSince1970 * 1_000)
        )
    }

    /// Opens `sealed` with any imported group whose recent-day `recipient_hint`
    /// matches `recipientHint`. Returns the matching group and opened payload,
    /// or nil. `openGroupMessage` does not check membership of the signer;
    /// callers must enforce that before trusting the body.
    private func tryOpenGroupMessage(recipientHint: Data, sealed: Data, now: Int64) -> (Group, OpenedMessage)? {
        let groups = (try? store.listGroups()) ?? []
        for group in groups {
            guard recentHintsFor(userId: group.id, now: now).contains(where: { $0 == recipientHint }) else { continue }
            if let opened = try? openGroupMessage(group: group, sealed: sealed) {
                return (group, opened)
            }
        }
        return nil
    }

    private func deliverOpened(
        sourceLabel: String,
        sourceAddress: String?,
        opened: OpenedMessage,
        identity: Identity,
        msgId: Data,
        arrival: MessageArrival
    ) throws {
        let extendedBody: ExtendedMessageBody
        do {
            extendedBody = try decodeExtendedMessageBody(bytes: opened.payload)
        } catch {
            // Undecodable body from a verified sender: deterministic reject,
            // terminal handled state (not a store failure).
            return
        }
        let body = MessageBody(
            kind: extendedBody.kind,
            chatId: extendedBody.chatId,
            lamport: extendedBody.lamport,
            timestamp: extendedBody.timestamp,
            content: extendedBody.content
        )
        guard body.chatId == opened.senderUserId else { return }
        let senderIsContact = (try? store.getContact(userId: opened.senderUserId)) != nil
        guard corePairwiseSenderAuthorized(
            kind: body.kind,
            senderIsContact: senderIsContact,
            senderIsSelf: opened.senderUserId == identity.userId
        ) else {
            log.warning("Dropping pairwise envelope from unauthorized sender on \(sourceLabel, privacy: .public)")
            return
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
            handleIncomingFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.profileSync:
            handleIncomingProfileSync(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.friendDirectory:
            handleIncomingFriendDirectory(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.introducedFriendRequest:
            handleIncomingIntroducedFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.lanEndpointHint:
            handleIncomingLanEndpointHint(
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
        ChatEvents.notifyChatChanged(group.id)

        // Local read watermark only (group wire receipts are deferred).
        let throughLamport = (try? store.highestLamport(chatId: group.id, senderUserId: senderUserId)) ?? 0
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
            let senderName = (try? store.getContact(userId: senderUserId))?.name
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
            let senderName = (try? store.getContact(userId: senderUserId))?.name
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

        let through = (try? store.highestContiguousLamport(chatId: senderUserId, senderUserId: senderUserId)) ?? 0
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
        // acked away before it is recorded.
        try store.recordReceipt(
            chatId: envelopeSender,
            senderUserId: identity.userId,
            receiptType: receipt.receiptType,
            throughLamport: receipt.lamport
        )
        _ = try? store.recordMessageArrival(
            chatId: envelopeSender,
            senderUserId: identity.userId,
            lamport: receipt.lamport,
            arrival: arrival
        )
        ChatEvents.notifyChatChanged(envelopeSender)
    }

    private func handleIncomingFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) {
        let pending = ((try? store.listFriendSuggestions(
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )) ?? []).first { $0.state == 1 && $0.candidate.userId == senderUserId }
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
        _ = try? store.upsertImportedContact(contact: contact)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }
        try? store.upsertContactProvenance(provenance: ContactProvenance(
            userId: senderUserId,
            source: pending == nil ? 0 : 1,
            introducerUserId: pending?.introducerUserId,
            introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ))
        if pending != nil { try? store.removeFriendSuggestion(candidateUserId: senderUserId) }
        ProfileSyncSender.queueToContact(
            store: store,
            identity: identity,
            contact: contact,
            displayName: ProfileStore.loadDisplayName(),
            epoch: ProfileStore.loadOwnAvatarEpoch()
        )
        let inserted = (try? store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.friendRequest,
            payload: body.content
        ))) ?? false
        guard inserted else { return }
        ChatEvents.notifyChatChanged(senderUserId)
        let through = (try? store.highestContiguousLamport(chatId: senderUserId, senderUserId: senderUserId)) ?? 0
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

    private func handleIncomingLanEndpointHint(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) {
        guard let contact = try? store.getContact(userId: senderUserId),
              let hint = try? decodeLanEndpointContent(bytes: body.content),
              let networkId = String(data: hint.networkId, encoding: .utf8) else { return }
        let endpoint = LanManualEndpoint(host: hint.host, port: hint.port)
        LanCapabilityStore.markSupported(userId: senderUserId)
        LanEndpointCache.save(networkId: networkId, userId: senderUserId, endpoint: endpoint)
        queueCurrentLanEndpoint(to: senderUserId)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }

        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        if hint.expiresAtMs > now, currentLanNetworkId == networkId {
            lanTransport?.connect(endpoint, remoteInstanceToken: hint.instanceToken)
        }

        let inserted = (try? store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.lanEndpointHint,
            payload: body.content
        ))) ?? false
        if inserted {
            acknowledgeHiddenMessage(
                sourceAddress: sourceAddress,
                senderUserId: senderUserId,
                identity: identity,
                contact: contact
            )
        }
    }

    private func handleIncomingProfileSync(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) {
        guard let existing = try? store.getContact(userId: senderUserId),
              let content = try? decodeProfileSyncContent(bytes: body.content) else { return }
        let inserted = (try? store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.profileSync,
            payload: body.content
        ))) ?? false
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
        let through = (try? store.highestContiguousLamport(chatId: senderUserId, senderUserId: senderUserId)) ?? 0
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
    }

    private func handleIncomingFriendDirectory(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) {
        guard let contact = try? store.getContact(userId: senderUserId),
              let content = try? decodeFriendDirectoryContent(bytes: body.content) else { return }
        let inserted = (try? store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: ProtocolKind.friendDirectory,
            payload: body.content
        ))) ?? false
        guard inserted else { return }
        if FriendsOfFriendsStore.isEnabled() {
            guard (try? store.applyFriendDirectory(
                introducerUserId: senderUserId,
                recipientUserId: identity.userId,
                content: content,
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

    private func handleIncomingIntroducedFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) {
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
        _ = try? store.upsertImportedContact(contact: contact)
        if let sourceAddress {
            sendLanEndpointHint(address: sourceAddress)
        }
        try? store.upsertContactProvenance(provenance: ContactProvenance(
            userId: senderUserId,
            source: 1,
            introducerUserId: introducer.userId,
            introducedAtMs: Int64(Date().timeIntervalSince1970 * 1_000)
        ))
        try? store.removeFriendSuggestion(candidateUserId: senderUserId)
        _ = try? store.insertMessage(message: StoredMessage(
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
        let through = (try? store.highestLamport(
            chatId: senderUserId,
            senderUserId: senderUserId
        )) ?? 0
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
        let hints = recentHintsFor(userId: group.id, now: now)
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
    private func carryForeign(
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        forceFamily: Bool = false
    ) -> Bool {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let isFamily = forceFamily || hintMatchesAnyContact(hint: recipientHint, now: now)
        guard let stored = try? store.enqueueCarriedEnvelope(
            envelope: CarriedEnvelope(
                msgId: msgId,
                hopTtl: hopTtl,
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
        let hints = deliveryHintsForPeer(peerUserId: peerUserId, now: now)
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
            peerHints: recentHintsFor(userId: peerUserId, now: now),
            peerKnownMsgIds: peerKnownIds,
            nowMs: now,
            ownOutboundBudgetBytes: MeshDefaults.ownOutboundSprayBudgetBytes,
            ownReceiptBudgetBytes: MeshDefaults.ownReceiptSprayBudgetBytes,
            receiptQueryLimit: MeshDefaults.relayBatchLimit
        ) else {
            log.warning("Failed to build digest spray plan for \(address, privacy: .public)")
            return
        }
        let frames = plan.carriedFrames + plan.ownOutboundFrames + plan.ownReceiptFrames
        for frame in frames {
            _ = MeshRouter.sendToAddress(address: address, frame: frame)
        }
    }

    private func hintMatchesAnyContact(hint: Data, now: Int64) -> Bool {
        let contacts = (try? store.listContacts()) ?? []
        for c in contacts {
            if recentHintsFor(userId: c.userId, now: now).contains(where: { $0 == hint }) {
                return true
            }
        }
        return recognizesGroupHint(hint, now: now)
    }

    private func deliveryHintsForPeer(peerUserId: Data, now: Int64) -> [Data] {
        var hints = recentHintsFor(userId: peerUserId, now: now)
        let groups = (try? store.listGroups()) ?? []
        for group in groups where group.memberUserIds.contains(peerUserId) {
            hints.append(contentsOf: recentHintsFor(userId: group.id, now: now))
        }
        return hints
    }

    private func recognizesGroupHint(_ hint: Data, now: Int64) -> Bool {
        let groups = (try? store.listGroups()) ?? []
        return groups.contains { group in
            recentHintsFor(userId: group.id, now: now).contains(hint)
        }
    }

    private func recentHintsFor(userId: Data, now: Int64) -> [Data] {
        (0...MeshDefaults.carryHintDayWindow).map { daysAgo in
            computeRecipientHint(recipientUserId: userId, timestampMs: now - daysAgo * MeshDefaults.msPerDay)
        }
    }

    // MARK: - Relay

    private func startRelayLoop() {
        relayTimer?.invalidate()
        relayTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.runRelaySync() }
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
        relayPushClient.start(config: config) {
            relayPushHints(ownUserId: ownUserId)
        }
    }

    private func runRelaySync() {
        guard isRunning, let identity else { return }
        guard let config = RelayConfigStore.load() else {
            MeshConnectivityStatus.shared.setRelayHealth(.noConfig)
            return
        }
        guard pathMonitor?.currentPath.status == .satisfied else {
            MeshConnectivityStatus.shared.setRelayHealth(.noInternet)
            return
        }
        if relaySyncInFlight {
            relaySyncPending = true
            return
        }
        relaySyncInFlight = true
        backfillRelayReceipts(identity: identity)
        if !pausedForBluetoothAudio {
            MeshRuntimeStatus.shared.markSyncingViaRelay()
        }
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
        let contacts = (try? store.listContacts()) ?? []
        for contact in contacts {
            for receiptType in [ReceiptType.delivered, ReceiptType.read] {
                let through = (try? store.outgoingReceiptThrough(
                    chatId: contact.userId,
                    senderUserId: contact.userId,
                    receiptType: receiptType
                )) ?? 0
                guard through > 0 else { continue }
                _ = queueOutgoingReceiptForRelay(
                    identity: identity,
                    contact: contact,
                    receiptType: receiptType,
                    ackedSenderUserId: contact.userId,
                    throughLamport: through
                )
            }
        }
    }

    private nonisolated func relaySyncBlocking(identity: Identity, config: RelayConfig) async {
        let store = AppStore.get()
        do {
            // Upload receipts first, then authored, then family carry.
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            _ = try store.pruneExpiredOutgoingReceiptEnvelopes(nowMs: now)
            _ = try store.pruneExpiredOutboundEnvelopes(nowMs: now)
            _ = try store.pruneExpiredCarried(nowMs: now)
            let receipts = try store.pendingRelayOutgoingReceiptEnvelopes(
                limit: MeshDefaults.relayBatchLimit,
                nowMs: now
            )
            for env in receipts {
                _ = try RelayClient.postReceiptEnvelope(config: config, envelope: env)
                _ = try store.markOutgoingReceiptEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
            }
            let outbound = try store.pendingRelayOutboundEnvelopes(
                limit: MeshDefaults.relayBatchLimit,
                nowMs: now
            )
            let importedGroups = try store.listGroups()
            let groupsById = Dictionary(
                uniqueKeysWithValues: importedGroups.map { ($0.id, $0) }
            )
            // Recent-day hints per imported group, for recognizing
            // group-hinted carried envelopes below (same window every other
            // hint check uses).
            let groupHintSets: [(group: Group, hints: Set<Data>)] = importedGroups.map { group in
                let hints = (0...MeshDefaults.carryHintDayWindow).map { daysAgo in
                    computeRecipientHint(
                        recipientUserId: group.id,
                        timestampMs: now - daysAgo * MeshDefaults.msPerDay
                    )
                }
                return (group, Set(hints))
            }
            for env in outbound {
                if let group = groupsById[env.recipientUserId] {
                    // Group-addressed: per-member fan-out instead of one
                    // shared group-hint row (specs/group-relay-durability.md
                    // §4.2). Mark relay-posted only after ALL member rows
                    // post; a partial failure retries the whole set next
                    // pass, and the deterministic fan-out msg_ids dedupe
                    // server-side. Mirrors MeshService.kt.
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
                        if (try? RelayClient.postFanoutRow(config: config, row: row)) != nil {
                            posted += 1
                        }
                    }
                    if posted == rows.count {
                        _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
                    }
                    continue
                }
                _ = try RelayClient.postOutboundEnvelope(config: config, envelope: env)
                _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
            }
            let family = try store.familyCarriedEnvelopes(
                limit: MeshDefaults.relayBatchLimit,
                nowMs: now
            )
            for env in family {
                // Group-hinted carried envelopes decompose into per-member
                // fan-out rows so a member mule's uplink serves the whole
                // group (specs/group-relay-durability.md §4.2); re-posts on
                // later passes dedupe server-side via the deterministic ids.
                // Everything else posts unchanged.
                if let match = groupHintSets.first(where: { $0.hints.contains(env.recipientHint) }) {
                    let rows = coreGroupFanoutRowsForCarried(
                        originalMsgId: env.msgId,
                        memberUserIds: match.group.memberUserIds,
                        hopTtl: env.hopTtl,
                        expiry: env.expiry,
                        sealed: env.sealed
                    )
                    for row in rows {
                        _ = try? RelayClient.postFanoutRow(config: config, row: row)
                    }
                    continue
                }
                _ = try RelayClient.postCarriedEnvelope(config: config, envelope: env)
            }

            let contacts = try store.listContacts()
            let presenceHints: (Data, Int64) -> [Data] = { userId, timestamp in
                (0...3).map { daysAgo in
                    computeRecipientHint(
                        recipientUserId: userId,
                        timestampMs: timestamp - Int64(daysAgo) * MeshDefaults.msPerDay
                    )
                }
            }
            let announce = RelayConfigStore.shareOnline()
                ? presenceHints(identity.userId, now)
                : []
            let query = Array(Set(contacts.flatMap { presenceHints($0.userId, now) }))
            if !announce.isEmpty || !query.isEmpty {
                let contactByHint = Dictionary(uniqueKeysWithValues: contacts.flatMap { contact in
                    presenceHints(contact.userId, now).map { ($0, contact.userId) }
                })
                let page = try RelayClient.syncPresence(
                    config: config,
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
                    }
                }
            }

            var hints: [Data] = [computeRecipientHint(
                recipientUserId: identity.userId,
                timestampMs: Int64(Date().timeIntervalSince1970 * 1000)
            )]
            for day in 1...MeshDefaults.carryHintDayWindow {
                hints.append(computeRecipientHint(
                    recipientUserId: identity.userId,
                    timestampMs: Int64(Date().timeIntervalSince1970 * 1000) - day * MeshDefaults.msPerDay
                ))
            }
            let groups = try store.listGroups()
            for group in groups where group.memberUserIds.contains(identity.userId) {
                hints.append(contentsOf: (0...MeshDefaults.carryHintDayWindow).map { daysAgo in
                    computeRecipientHint(
                        recipientUserId: group.id,
                        timestampMs: now - daysAgo * MeshDefaults.msPerDay
                    )
                })
            }
            var afterId: Int64 = 0
            while true {
                let page = try RelayClient.fetchEnvelopes(
                    config: config,
                    hints: hints,
                    afterId: afterId,
                    limit: Int(MeshDefaults.relayBatchLimit)
                )
                guard !page.envelopes.isEmpty else { break }
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
                }
                // Consumed/Expired ack unconditionally; a SEEN envelope is
                // acked only if this device durably consumed it as a 1:1
                // message from someone else (DTN_TODOS.md §3.1); a legacy
                // shared-mailbox group-hint row is never acked at all
                // (specs/group-relay-durability.md §5.2) -- see
                // CoreRelayEnvelopeDisposition's doc comment in engine.rs.
                let acks = try store.coreRelayAckIdsWithConsumed(
                    items: dispositions,
                    ownUserId: identity.userId,
                    nowMs: now
                )
                if !acks.isEmpty {
                    try RelayClient.ackEnvelopes(config: config, ids: acks)
                }
                afterId = page.nextCursor
                if page.envelopes.count < Int(MeshDefaults.relayBatchLimit) { break }
            }
            await MainActor.run {
                MeshConnectivityStatus.shared.setRelayHealth(.ok(
                    lastSyncMs: Int64(Date().timeIntervalSince1970 * 1_000)
                ))
            }
        } catch {
            let message = error.localizedDescription
            await MainActor.run {
                MeshConnectivityStatus.shared.setRelayHealth(.failing(
                    lastAttemptMs: Int64(Date().timeIntervalSince1970 * 1_000)
                ))
                log.warning("Relay sync failed: \(message, privacy: .public)")
            }
        }
    }

    private func refreshNearby() {
        guard isRunning else { return }
        MeshConnectivityStatus.shared.refreshNearbyRoutes()
        if pausedForBluetoothAudio {
            MeshRuntimeStatus.shared.markPausedForBluetoothAudio()
        } else {
            MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
        }
    }
}

/// Self + owned-group recipient hints for the current moment -- the same hint
/// set `MeshController.relaySyncBlocking` computes inline for its own relay
/// fetch. A free function (not a `MeshController` method) so it carries no
/// main-actor isolation: `RelayPushClient` (DTN_TODOS.md D3) invokes its
/// `hintsProvider` closure from its own private queue, off the main actor,
/// the same reason `relaySyncBlocking` itself is `nonisolated` and
/// duplicates this computation inline rather than calling the
/// `@MainActor`-isolated `recentHintsFor`/`deliveryHintsForPeer` helpers.
/// Unlike Android's `relayHintsForConfig`/`relayProxyHints`, there is no
/// "proxy" hint set here (mail addressed to a BLE-only contact, fetched on
/// their behalf) -- the iOS poll path doesn't compute those either, so there
/// is nothing to mirror for that half yet.
private func relayPushHints(ownUserId: Data) -> [Data] {
    let store = AppStore.get()
    let now = Int64(Date().timeIntervalSince1970 * 1000)
    var hints = (0...MeshDefaults.carryHintDayWindow).map { daysAgo in
        computeRecipientHint(recipientUserId: ownUserId, timestampMs: now - daysAgo * MeshDefaults.msPerDay)
    }
    let groups = (try? store.listGroups()) ?? []
    for group in groups where group.memberUserIds.contains(ownUserId) {
        hints.append(contentsOf: (0...MeshDefaults.carryHintDayWindow).map { daysAgo in
            computeRecipientHint(recipientUserId: group.id, timestampMs: now - daysAgo * MeshDefaults.msPerDay)
        })
    }
    return hints
}
