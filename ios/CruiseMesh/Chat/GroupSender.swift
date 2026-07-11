import Foundation
import os.log

/// Creates groups, fans out pairwise `kind=4` invites, and authors group text
/// (DESIGN.md §6.5). Group text is sealed once with the shared group key and
/// flooded; invites are one pairwise-sealed envelope per other member so each
/// recipient can import the group key under their existing 1:1 crypto.
final class GroupSender {
    private let store: MessageStore
    private let identity: Identity
    private let log = Logger(subsystem: "com.cruisemesh", category: "GroupSender")

    init(store: MessageStore, identity: Identity) {
        self.store = store
        self.identity = identity
    }

    /// Creates a group containing `identity` plus `members`, persists it, and
    /// queues a pairwise invite to every other member. Returns the created
    /// `Group`, or nil if creation failed / no members selected.
    @discardableResult
    func createAndInvite(name: String, members: [Contact]) -> Group? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            log.warning("Refusing to create a group with an empty name")
            return nil
        }
        guard !members.isEmpty else {
            log.warning("Refusing to create a group with no other members")
            return nil
        }

        var memberIds: [Data] = [identity.userId]
        for member in members where !memberIds.contains(member.userId) {
            memberIds.append(member.userId)
        }

        let group: Group
        do {
            group = try createGroup(name: trimmed, memberUserIds: memberIds)
            try store.upsertGroup(group: group)
        } catch {
            log.warning("Failed to create or persist group: \(error.localizedDescription, privacy: .public)")
            return nil
        }

        queueInvites(group: group, members: members)
        ChatEvents.notifyChatChanged(group.id)
        RelaySyncEvents.requestSync()
        return group
    }

    /// Sends `text` into `group`'s chat stream, sealed with the group key.
    func sendText(group: Group, text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let payload = Data(trimmed.utf8)
        guard !payload.isEmpty, group.memberUserIds.contains(identity.userId) else { return }

        enqueueGroupMessage(group: group, kind: ProtocolKind.text, payload: payload, label: "sendText")
    }

    func sendReaction(group: Group, target: MessageTarget, emoji: String) {
        guard group.memberUserIds.contains(identity.userId) else { return }
        enqueueGroupMessage(
            group: group,
            kind: ProtocolKind.reaction,
            payload: ReactionPayload(target: target, emoji: emoji).encode(),
            label: "sendReaction"
        )
    }

    private func enqueueGroupMessage(group: Group, kind: UInt8, payload: Data, label: String) {
        let chatId = group.id
        let lamport = (try? store.highestContiguousLamport(chatId: chatId, senderUserId: identity.userId)) ?? 0
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let message = StoredMessage(
            chatId: chatId,
            senderUserId: identity.userId,
            lamport: lamport + 1,
            timestamp: timestamp,
            kind: kind,
            payload: payload
        )
        guard let outbound = buildOutboundGroupEnvelope(identity: identity, group: group, message: message) else {
            return
        }
        _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
        ChatEvents.notifyChatChanged(chatId)
        RelaySyncEvents.requestSync()

        let frame = encodeOutboundEnvelopeFrame(outbound)
        let fanout = MeshRouter.relayToAll(frame: frame)
        if fanout == 0 {
            log.info("\(label, privacy: .public): no live links; group message stays local for carry/digest/relay")
        } else {
            log.info("\(label, privacy: .public): flooded group message to \(fanout, privacy: .public) link(s)")
        }
    }

    /// One pairwise-sealed invite per other member. Local history stores a
    /// single `kind=4` row under `chat_id = group.id`; the outbound queue holds
    /// N sealed envelopes keyed by recipient.
    private func queueInvites(group: Group, members: [Contact]) {
        let inviteContent: Data
        do {
            inviteContent = try encodeGroupInviteContent(group: group)
        } catch {
            log.warning("encodeGroupInviteContent failed: \(error.localizedDescription, privacy: .public)")
            return
        }
        let lamport = (try? store.highestContiguousLamport(chatId: group.id, senderUserId: identity.userId)) ?? 0
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let message = StoredMessage(
            chatId: group.id,
            senderUserId: identity.userId,
            lamport: lamport + 1,
            timestamp: timestamp,
            kind: ProtocolKind.groupInvite,
            payload: inviteContent
        )

        var anyQueued = false
        for member in members where member.userId != identity.userId {
            guard let outbound = buildOutboundAuthoredEnvelope(
                identity: identity,
                contact: member,
                message: message
            ) else { continue }
            _ = try? store.insertOutgoingMessage(message: message, envelope: outbound, queuedAtMs: timestamp)
            anyQueued = true
            if !MeshRouter.sendToUserId(userId: member.userId, frame: encodeOutboundEnvelopeFrame(outbound)) {
                log.info("Queued group invite for \(member.name, privacy: .public); peer not currently connected")
            }
        }
        if anyQueued {
            ChatEvents.notifyChatChanged(group.id)
        }
    }
}
