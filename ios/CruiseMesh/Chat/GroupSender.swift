import Foundation
import os.log

final class GroupSender {
    private let store: MessageStore
    private let identity: Identity
    private let log = Logger(subsystem: "com.cruisemesh", category: "GroupSender")

    init(store: MessageStore, identity: Identity) {
        self.store = store
        self.identity = identity
    }

    func createAndInvite(name: String, members: [Contact]) -> Group? {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty, !members.isEmpty else { return nil }

        var memberIds = [identity.userId]
        for member in members where !memberIds.contains(member.userId) {
            memberIds.append(member.userId)
        }

        do {
            let group = try createGroup(name: trimmedName, memberUserIds: memberIds)
            try store.upsertGroup(group: group)
            queueInvites(group: group, members: members)
            ChatEvents.notifyChatChanged(group.id)
            RelaySyncEvents.requestSync()
            return group
        } catch {
            log.warning("Failed to create group: \(error.localizedDescription, privacy: .public)")
            return nil
        }
    }

    func sendText(group: Group, text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, group.memberUserIds.contains(identity.userId) else { return }

        let lamport = ((try? store.highestContiguousLamport(
            chatId: group.id,
            senderUserId: identity.userId
        )) ?? 0) + 1
        let timestamp = Int64(Date().timeIntervalSince1970 * 1_000)
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: identity.userId,
            lamport: lamport,
            timestamp: timestamp,
            kind: ProtocolKind.text,
            payload: Data(trimmed.utf8)
        )
        guard let outbound = buildOutboundGroupEnvelope(
            identity: identity,
            group: group,
            message: message
        ) else { return }

        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        ChatEvents.notifyChatChanged(group.id)
        RelaySyncEvents.requestSync()
        _ = MeshRouter.relayToAll(frame: encodeOutboundEnvelopeFrame(outbound))
    }

    private func queueInvites(group: Group, members: [Contact]) {
        guard let inviteContent = try? encodeGroupInviteContent(group: group) else { return }
        let lamport = ((try? store.highestContiguousLamport(
            chatId: group.id,
            senderUserId: identity.userId
        )) ?? 0) + 1
        let timestamp = Int64(Date().timeIntervalSince1970 * 1_000)
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: identity.userId,
            lamport: lamport,
            timestamp: timestamp,
            kind: ProtocolKind.groupInvite,
            payload: inviteContent
        )

        for member in members where member.userId != identity.userId {
            guard let outbound = buildOutboundAuthoredEnvelope(
                identity: identity,
                contact: member,
                message: message
            ) else { continue }
            _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
            _ = MeshRouter.sendToUserId(
                userId: member.userId,
                frame: encodeOutboundEnvelopeFrame(outbound)
            )
        }
    }
}
