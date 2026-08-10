import SwiftUI
import UIKit

enum ChatListLogic {
    static func contactDisplayName(_ contact: Contact) -> String {
        displayNameOrId(
            name: coreContactDisplayName(contact: contact),
            displayId: formatUserId(userId: contact.userId)
        )
    }

    static func displayNameOrId(name: String, displayId: String) -> String {
        if !name.isEmpty && name != "Unknown" { return name }
        return displayId
    }

    /// Avatar colour, plus the initials to draw inside it.
    ///
    /// The initials are empty whenever there is no usable name: `AvatarView`
    /// then draws a neutral person glyph instead. This used to fall back to the
    /// first two characters of the user id, which rendered as `7B` — meaningless
    /// to anyone reading it, and on the onboarding profile slide it was the
    /// first thing a new tester ever saw of their own identity, before they had
    /// typed a name. PR #178 fixed the contact half of the same problem.
    ///
    /// `displayId` is kept in the signature for the call sites that still pass
    /// it; nothing derived from an identifier is shown to a person any more.
    static func avatarHueAndInitials(userId: Data, name: String, displayId: String) -> (Color, String) {
        let sum = userId.reduce(0) { $0 + Int($1) }
        let hue = Double(sum & 0xFF) / 255.0
        let color = Color(hue: hue, saturation: 0.5, brightness: 0.7)
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let initials: String
        if !trimmed.isEmpty && trimmed != "Unknown" {
            initials = String(trimmed.prefix(2)).uppercased()
        } else {
            initials = ""
        }
        return (color, initials)
    }

    static func formatRelativeTime(timestampMs: Int64, nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) -> String {
        let now = Date(timeIntervalSince1970: TimeInterval(nowMs) / 1000)
        let then = Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1000)
        let cal = Calendar.current
        if cal.isDate(now, inSameDayAs: then) {
            let f = DateFormatter()
            f.dateFormat = "h:mm a"
            f.locale = .current
            return f.string(from: then)
        }
        let diff = nowMs - timestampMs
        if diff >= 0 && diff < 7 * 24 * 60 * 60 * 1000 {
            let f = DateFormatter()
            f.dateFormat = "EEE"
            f.locale = .current
            return f.string(from: then)
        }
        let f = DateFormatter()
        f.dateFormat = "MMM d"
        f.locale = .current
        return f.string(from: then)
    }

    static func computeUnread(messages: [StoredMessage], ownUserId: Data, readThrough: UInt64) -> Int {
        Int(coreUnreadCount(messages: messages, ownUserId: ownUserId, readThrough: readThrough))
    }

    /**
     Group unread across multi-sender streams: for each other sender, count
     messages above our local READ watermark for that sender.
     */
    static func computeGroupUnread(
        messages: [StoredMessage],
        ownUserId: Data,
        readThroughForSender: (Data) -> UInt64
    ) -> Int {
        messages.filter { msg in
            isVisibleChatKind(msg.kind)
                && msg.senderUserId != ownUserId
                && msg.lamport > readThroughForSender(msg.senderUserId)
        }.count
    }

    static func lastVisibleMessage(_ messages: [StoredMessage]) -> StoredMessage? {
        guard let selected = coreLastVisibleMessage(messages: messages) else { return nil }
        return messages.first {
            $0.chatId == selected.chatId && $0.senderUserId == selected.senderUserId
                && $0.lamport == selected.lamport && $0.kind == selected.kind
        }
    }

    static func previewText(_ message: StoredMessage, groupName: String? = nil) -> String {
        switch message.kind {
        case ProtocolKind.attachmentManifest:
            return AttachmentPayload.previewLabel(AttachmentPayload.decode(message.payload))
        case ProtocolKind.groupInvite:
            if let groupName { return "Group created: \(groupName)" }
            return "Group invite"
        default:
            return String(data: message.payload, encoding: .utf8) ?? ""
        }
    }
}
