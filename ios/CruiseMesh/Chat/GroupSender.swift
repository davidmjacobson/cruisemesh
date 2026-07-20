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
    func sendText(group: Group, text: String, replyToMsgId: Data? = nil) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let payload = Data(trimmed.utf8)
        guard !payload.isEmpty, group.memberUserIds.contains(identity.userId) else { return }

        enqueueGroupMessage(
            group: group,
            kind: ProtocolKind.text,
            payload: payload,
            label: "sendText",
            replyToMsgId: replyToMsgId
        )
    }

    func sendAttachment(
        group: Group,
        attachment: AttachmentPayload,
        replyToMsgId: Data? = nil
    ) {
        guard group.memberUserIds.contains(identity.userId),
              attachment.blob.count <= AttachmentPayload.maxBlobBytes else { return }
        enqueueGroupMessage(
            group: group,
            kind: ProtocolKind.attachmentManifest,
            payload: attachment.encode(),
            label: "sendAttachment",
            replyToMsgId: replyToMsgId
        )
    }

    func sendReaction(group: Group, target: MessageTarget, emoji: String) {
        guard group.memberUserIds.contains(identity.userId) else { return }
        guard let payload = ReactionPayload(target: target, emoji: emoji).encode() else { return }
        enqueueGroupMessage(
            group: group,
            kind: ProtocolKind.reaction,
            payload: payload,
            label: "sendReaction"
        )
    }

    @discardableResult
    func renameGroup(group: Group, name: String) -> Group? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed != group.name else { return nil }
        do {
            let result = try store.authorGroupMetadataUpdate(
                identity: identity,
                group: group,
                name: trimmed,
                memberUserIds: group.memberUserIds,
                timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
            )
            publishGroupFrame(label: "renameGroup", authored: result.authored)
            ChatEvents.notifyChatChanged(group.id)
            return result.group
        } catch {
            log.error("renameGroup: metadata update was not stored: \(error.localizedDescription, privacy: .public)")
            return nil
        }
    }

    @discardableResult
    func addMembers(group: Group, additions: [Contact]) -> Group? {
        var seen = Set(group.memberUserIds)
        let newMembers = additions.filter { seen.insert($0.userId).inserted }
        guard !newMembers.isEmpty else { return nil }
        do {
            let result = try store.authorGroupMetadataUpdate(
                identity: identity,
                group: group,
                name: group.name,
                memberUserIds: group.memberUserIds + newMembers.map(\.userId),
                timestampMs: Int64(Date().timeIntervalSince1970 * 1_000)
            )
            // Queue pairwise key-bearing invitations before the live metadata
            // flood. Both paths remain durable when members are offline.
            queueInvites(group: result.group, members: newMembers)
            publishGroupFrame(label: "addMembers", authored: result.authored)
            ChatEvents.notifyChatChanged(group.id)
            return result.group
        } catch {
            log.error("addMembers: metadata update was not stored: \(error.localizedDescription, privacy: .public)")
            return nil
        }
    }

    private func enqueueGroupMessage(
        group: Group,
        kind: UInt8,
        payload: Data,
        label: String,
        replyToMsgId: Data? = nil
    ) {
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        guard let authored = try? store.authorGroupMessage(
            identity: identity, group: group, kind: kind, payload: payload,
            replyToMsgId: replyToMsgId, timestampMs: timestamp
        ) else {
            return
        }
        // V2 field metric: note the outbound group send for the cruise-test export.
        try? store.recordSentMetric(
            chatId: authored.message.chatId,
            lamport: authored.message.lamport,
            sentAtMs: timestamp
        )
        ChatEvents.notifyChatChanged(authored.message.chatId)
        publishGroupFrame(label: label, authored: authored)
    }

    private func publishGroupFrame(label: String, authored: AuthoredEnvelope) {
        RelaySyncEvents.requestSync()
        let fanout = MeshRouter.relayToAll(frame: authored.frame)
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
        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        guard let authored = try? store.queueGroupInvites(
            identity: identity, group: group, members: members, timestampMs: timestamp
        ) else { return }
        for invite in authored {
            if !MeshRouter.sendToUserId(userId: invite.envelope.recipientUserId, frame: invite.frame) {
                log.info("Queued group invite; peer not currently connected")
            }
        }
        if !authored.isEmpty {
            ChatEvents.notifyChatChanged(group.id)
        }
    }
}
