import Foundation
import os.log

private let lanEndpointHintLifetimeMs: Int64 = 15 * 60 * 1_000

enum LanEndpointSender {
    private static let log = Logger(subsystem: "com.cruisemesh", category: "LanEndpointSender")

    static func queueToAllCapableContacts(
        store: MessageStore,
        identity: Identity,
        endpoint: LanManualEndpoint,
        instanceToken: Data,
        networkId: String?
    ) {
        guard let networkId else { return }
        for contact in (try? store.listContacts()) ?? [] where LanCapabilityStore.isSupported(userId: contact.userId) {
            queueToContact(
                store: store,
                identity: identity,
                contact: contact,
                endpoint: endpoint,
                instanceToken: instanceToken,
                networkId: networkId
            )
        }
    }

    static func queueToContact(
        store: MessageStore,
        identity: Identity,
        contact: Contact,
        endpoint: LanManualEndpoint,
        instanceToken: Data,
        networkId: String
    ) {
        guard LanCapabilityStore.shouldSendEndpoint(
            userId: contact.userId,
            networkId: networkId,
            endpoint: endpoint,
            instanceToken: instanceToken
        ) else { return }

        let own = (try? store.highestContiguousLamport(
            chatId: contact.userId,
            senderUserId: identity.userId
        )) ?? 0
        let delivered = (try? store.receiptThrough(
            chatId: contact.userId,
            senderUserId: identity.userId,
            receiptType: ReceiptType.delivered
        )) ?? 0
        let read = (try? store.receiptThrough(
            chatId: contact.userId,
            senderUserId: identity.userId,
            receiptType: ReceiptType.read
        )) ?? 0
        let timestamp = Int64(Date().timeIntervalSince1970 * 1_000)
        let payload: Data
        do {
            payload = try encodeLanEndpointContent(content: LanEndpointContent(
                instanceToken: instanceToken,
                networkId: Data(networkId.utf8),
                host: endpoint.host,
                port: endpoint.port,
                expiresAtMs: timestamp + lanEndpointHintLifetimeMs
            ))
        } catch {
            log.warning("Unable to encode sealed LAN endpoint hint")
            return
        }
        let message = StoredMessage(
            chatId: contact.userId,
            senderUserId: identity.userId,
            lamport: max(max(own, delivered), read) + 1,
            timestamp: timestamp,
            kind: ProtocolKind.lanEndpointHint,
            payload: payload
        )
        guard let outbound = buildOutboundAuthoredEnvelope(
            identity: identity,
            contact: contact,
            message: message
        ) else { return }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        RelaySyncEvents.requestSync()
        _ = MeshRouter.sendToUserId(userId: contact.userId, frame: encodeOutboundEnvelopeFrame(outbound))
    }
}
