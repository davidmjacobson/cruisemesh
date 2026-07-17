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
        guard let authored = try? store.authorPairwiseMessage(
            identity: identity,
            contact: contact,
            kind: ProtocolKind.lanEndpointHint,
            payload: payload,
            replyToMsgId: nil,
            timestampMs: timestamp
        ) else { return }
        GossipState.seenIds.record(msgId: authored.envelope.msgId)
        RelaySyncEvents.requestSync()
        _ = MeshRouter.sendToUserId(userId: contact.userId, frame: authored.frame)
    }
}
