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
    private let store = AppStore.get()
    private let bluetoothAudioBackoff = BluetoothAudioBackoff()
    private var identity: Identity!
    private var relayCursor: Int64 = 0
    private var relayTimer: Timer?
    private var pathMonitor: NWPathMonitor?
    private var isRunning = false
    private var meshRolesRunning = false
    private var pausedForBluetoothAudio = false
    private var relayCancellable: AnyCancellable?
    private var audioRouteObserver: NSObjectProtocol?

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
        stopMeshRoles()
        MeshRouter.unregisterCentral()
        MeshRouter.unregisterPeripheral()
        MeshRouter.reset()
        relayTimer?.invalidate()
        relayTimer = nil
        pathMonitor?.cancel()
        pathMonitor = nil
        relayCancellable?.cancel()
        relayCancellable = nil
        MeshRuntimeStatus.shared.markStopped()
        log.info("Mesh stopped")
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
        MeshRouter.reset()
    }

    func notifyChatViewed(chatId: Data) {
        guard let identity else { return }
        guard let contact = try? store.getContact(userId: chatId) else { return }
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
        }
    }

    private func handleHello(address: String, userId: Data, identity: Identity) {
        MeshRouter.onHello(address: address, userId: userId)
        log.info("HELLO from \(address, privacy: .public) \(UserIdHex.encode(userId), privacy: .public)")
        drainCarriedEnvelopesTo(address: address, peerUserId: userId)
        let entries: [DigestEntry]
        if let contact = try? store.getContact(userId: userId) {
            entries = (try? store.chatDigest(chatId: contact.userId)) ?? []
        } else {
            entries = []
        }
        let carried = (try? store.carriedMsgIds(limit: MeshDefaults.digestCarriedMsgIdsLimit)) ?? []
        let digest = encodeDigest(chatId: identity.userId, entries: entries, recentMsgIds: carried)
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
        sprayCarriedEnvelopesTo(address: address, peerUserId: peerUserId, peerKnownIds: Set(recentMsgIds))
    }

    func processInboundEnvelope(
        sourceAddress: String?,
        msgId: Data,
        hopTtl: UInt8,
        expiry: Int64,
        recipientHint: Data,
        sealed: Data,
        identity: Identity
    ) {
        let sourceLabel = sourceAddress ?? "relay"
        guard GossipState.seenIds.checkAndRecord(msgId: msgId) else { return }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        if expiry <= now {
            log.info("Dropping expired envelope from \(sourceLabel, privacy: .public)")
            return
        }
        do {
            let opened = try openMessage(recipient: identity, sealed: sealed)
            deliverOpened(sourceLabel: sourceLabel, sourceAddress: sourceAddress, opened: opened, identity: identity)
        } catch {
            relayForeign(
                sourceAddress: sourceAddress,
                msgId: msgId,
                hopTtl: hopTtl,
                expiry: expiry,
                recipientHint: recipientHint,
                sealed: sealed
            )
            carryForeign(msgId: msgId, hopTtl: hopTtl, expiry: expiry, recipientHint: recipientHint, sealed: sealed)
        }
    }

    private func deliverOpened(
        sourceLabel: String,
        sourceAddress: String?,
        opened: OpenedMessage,
        identity: Identity
    ) {
        let body: MessageBody
        do {
            body = try decodeMessageBody(bytes: opened.payload)
        } catch {
            return
        }
        guard body.chatId == opened.senderUserId else { return }

        switch body.kind {
        case ProtocolKind.text, ProtocolKind.attachmentManifest:
            handleIncomingChat(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity,
                kind: body.kind
            )
        case ProtocolKind.receipt:
            handleIncomingReceipt(
                sourceAddress: sourceAddress,
                envelopeSender: opened.senderUserId,
                body: body,
                identity: identity
            )
        case ProtocolKind.friendRequest:
            handleIncomingFriendRequest(
                sourceAddress: sourceAddress,
                senderUserId: opened.senderUserId,
                body: body,
                identity: identity
            )
        default:
            log.info("Unhandled kind=\(body.kind) from \(sourceLabel, privacy: .public)")
        }
    }

    private func handleIncomingChat(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity,
        kind: UInt8
    ) {
        let inserted = (try? store.insertMessage(message: StoredMessage(
            chatId: senderUserId,
            senderUserId: senderUserId,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: kind,
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
        if !visible {
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
        identity: Identity
    ) {
        guard let receipt = try? decodeReceiptContent(bytes: body.content) else { return }
        guard receipt.senderUserId == identity.userId else { return }
        guard (try? store.getContact(userId: envelopeSender)) != nil else { return }
        try? store.recordReceipt(
            chatId: envelopeSender,
            senderUserId: identity.userId,
            receiptType: receipt.receiptType,
            throughLamport: receipt.lamport
        )
        ChatEvents.notifyChatChanged(envelopeSender)
    }

    private func handleIncomingFriendRequest(
        sourceAddress: String?,
        senderUserId: Data,
        body: MessageBody,
        identity: Identity
    ) {
        guard let json = String(data: body.content, encoding: .utf8),
              let card = try? parseFriendCard(json: json),
              friendCardUserId(card: card) == senderUserId else { return }
        let contact = Contact(
            userId: senderUserId,
            name: card.name,
            signPk: card.signPk,
            agreePk: card.agreePk,
            relayUrl: card.relayUrl,
            relayToken: card.relayToken
        )
        try? store.upsertContact(contact: contact)
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
        log.info("Imported contact \(contact.name, privacy: .public) from friend request")
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
        guard let frame = buildReceiptFrame(
            identity: identity,
            contact: contact,
            receiptType: receiptType,
            ackedSenderUserId: ackedSenderUserId,
            throughLamport: throughLamport
        ) else { return }
        MeshRouter.sendToAddress(address: address, frame: frame)
    }

    private func sendReceiptToContact(
        identity: Identity,
        contact: Contact,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) {
        guard let frame = buildReceiptFrame(
            identity: identity,
            contact: contact,
            receiptType: receiptType,
            ackedSenderUserId: ackedSenderUserId,
            throughLamport: throughLamport
        ) else { return }
        _ = MeshRouter.sendToUserId(userId: contact.userId, frame: frame)
    }

    private func buildReceiptFrame(
        identity: Identity,
        contact: Contact,
        receiptType: UInt8,
        ackedSenderUserId: Data,
        throughLamport: UInt64
    ) -> Data? {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let content = encodeReceiptContent(content: ReceiptContent(
            chatId: identity.userId,
            senderUserId: ackedSenderUserId,
            lamport: throughLamport,
            receiptType: receiptType
        ))
        let body = MessageBody(
            kind: ProtocolKind.receipt,
            chatId: identity.userId,
            lamport: 0,
            timestamp: timestamp,
            content: content
        )
        do {
            let sealed = try sealMessage(
                sender: identity,
                recipientAgreePk: contact.agreePk,
                payload: encodeMessageBody(body: body)
            )
            let msgId = generateMsgId()
            GossipState.seenIds.record(msgId: msgId)
            return encodeEnvelopeFrame(
                msgId: msgId,
                hopTtl: MeshDefaults.hopTtl,
                expiry: defaultExpiry(timestampMs: timestamp),
                recipientHint: computeRecipientHint(recipientUserId: contact.userId, timestampMs: timestamp),
                sealed: sealed
            )
        } catch {
            return nil
        }
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
        let content = encodeReceiptContent(content: ReceiptContent(
            chatId: identity.userId,
            senderUserId: ackedSenderUserId,
            lamport: throughLamport,
            receiptType: receiptType
        ))
        let body = MessageBody(
            kind: ProtocolKind.receipt,
            chatId: identity.userId,
            lamport: 0,
            timestamp: timestamp,
            content: content
        )
        do {
            let sealed = try sealMessage(
                sender: identity,
                recipientAgreePk: contact.agreePk,
                payload: encodeMessageBody(body: body)
            )
            let msgId = generateMsgId()
            GossipState.seenIds.record(msgId: msgId)
            let envelope = OutgoingReceiptEnvelope(
                msgId: msgId,
                recipientUserId: contact.userId,
                chatId: contact.userId,
                senderUserId: ackedSenderUserId,
                receiptType: receiptType,
                throughLamport: throughLamport,
                timestamp: timestamp,
                hopTtl: MeshDefaults.hopTtl,
                expiry: defaultExpiry(timestampMs: timestamp),
                recipientHint: computeRecipientHint(recipientUserId: contact.userId, timestampMs: timestamp),
                sealed: sealed
            )
            return (try? store.upsertOutgoingReceiptEnvelope(envelope: envelope, queuedAtMs: timestamp)) ?? false
        } catch {
            return false
        }
    }

    private func backfillOutbound(identity: Identity, contact: Contact, message: StoredMessage) -> OutboundEnvelope? {
        guard let outbound = buildOutboundAuthoredEnvelope(identity: identity, contact: contact, message: message) else {
            return nil
        }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: message.timestamp)
        return outbound
    }

    private func carryForeign(msgId: Data, hopTtl: UInt8, expiry: Int64, recipientHint: Data, sealed: Data) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let isFamily = hintMatchesAnyContact(hint: recipientHint, now: now)
        _ = try? store.enqueueCarriedEnvelope(
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
        )
        if isFamily { RelaySyncEvents.requestSync() }
    }

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

    private func drainCarriedEnvelopesTo(address: String, peerUserId: Data) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        try? store.pruneExpiredCarried(nowMs: now)
        let hints = recentHintsFor(userId: peerUserId, now: now)
        let toDeliver = (try? store.carriedEnvelopesForHints(hints: hints, nowMs: now)) ?? []
        for env in toDeliver {
            let frame = encodeEnvelopeFrame(
                msgId: env.msgId,
                hopTtl: env.hopTtl,
                expiry: env.expiry,
                recipientHint: env.recipientHint,
                sealed: env.sealed
            )
            if MeshRouter.sendToAddress(address: address, frame: frame) {
                try? store.removeCarriedEnvelope(msgId: env.msgId)
            }
        }
    }

    private func sprayCarriedEnvelopesTo(address: String, peerUserId: Data, peerKnownIds: Set<Data>) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let peerHints = recentHintsFor(userId: peerUserId, now: now)
        let candidates = (try? store.carriedEnvelopesForPeerSync(
            peerHints: peerHints,
            peerKnownMsgIds: Array(peerKnownIds),
            nowMs: now
        )) ?? []
        for env in candidates.prefix(32) {
            let frame = encodeEnvelopeFrame(
                msgId: env.msgId,
                hopTtl: env.hopTtl,
                expiry: env.expiry,
                recipientHint: env.recipientHint,
                sealed: env.sealed
            )
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
        return false
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
        }
        pathMonitor?.start(queue: .global(qos: .utility))

        // Immediate kick on send
        relayCancellable = RelaySyncEvents.subject.sink { [weak self] in
            Task { @MainActor in self?.runRelaySync() }
        }
    }

    private func runRelaySync() {
        guard let identity, let config = RelayConfigStore.load() else { return }
        if !pausedForBluetoothAudio {
            MeshRuntimeStatus.shared.markSyncingViaRelay()
        }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            self?.relaySyncBlocking(identity: identity, config: config)
            Task { @MainActor in
                self?.refreshNearby()
            }
        }
    }

    private nonisolated func relaySyncBlocking(identity: Identity, config: RelayConfig) {
        let store = AppStore.get()
        do {
            // Upload receipts first, then authored, then family carry.
            let now = Int64(Date().timeIntervalSince1970 * 1000)
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
            for env in outbound {
                _ = try RelayClient.postOutboundEnvelope(config: config, envelope: env)
                _ = try store.markOutboundEnvelopeRelayPosted(msgId: env.msgId, postedAtMs: now)
            }
            let family = try store.familyCarriedEnvelopes(
                limit: MeshDefaults.relayBatchLimit,
                nowMs: now
            )
            for env in family {
                _ = try RelayClient.postCarriedEnvelope(config: config, envelope: env)
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
            let page = try RelayClient.fetchEnvelopes(
                config: config,
                hints: hints,
                afterId: 0,
                limit: Int(MeshDefaults.relayBatchLimit)
            )
            var acks: [Int64] = []
            for env in page.envelopes {
                Task { @MainActor in
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
                acks.append(env.id)
            }
            try RelayClient.ackEnvelopes(config: config, ids: acks)
        } catch {
            // Best-effort; next poll retries.
        }
    }

    private func refreshNearby() {
        guard isRunning else { return }
        if pausedForBluetoothAudio {
            MeshRuntimeStatus.shared.markPausedForBluetoothAudio()
        } else {
            MeshRuntimeStatus.shared.markMeshing(nearby: MeshRouter.connectedUserCount())
        }
    }
}
