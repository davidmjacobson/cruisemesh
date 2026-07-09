import SwiftUI
import UIKit

enum ChatListLogic {
    static func displayNameOrId(name: String, displayId: String) -> String {
        if !name.isEmpty && name != "Unknown" { return name }
        return displayId
    }

    static func avatarHueAndInitials(userId: Data, name: String, displayId: String) -> (Color, String) {
        let sum = userId.reduce(0) { $0 + Int($1) }
        let hue = Double(sum & 0xFF) / 255.0
        let color = Color(hue: hue, saturation: 0.5, brightness: 0.7)
        let initials: String
        if !name.isEmpty && name != "Unknown" {
            initials = String(name.prefix(2)).uppercased()
        } else {
            let cleaned = displayId.hasPrefix("CM-") ? String(displayId.dropFirst(3)) : displayId
            initials = String(cleaned.prefix(2)).uppercased()
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
            f.locale = Locale(identifier: "en_US")
            return f.string(from: then)
        }
        let diff = nowMs - timestampMs
        if diff >= 0 && diff < 7 * 24 * 60 * 60 * 1000 {
            let f = DateFormatter()
            f.dateFormat = "EEE"
            f.locale = Locale(identifier: "en_US")
            return f.string(from: then)
        }
        let f = DateFormatter()
        f.dateFormat = "MMM d"
        f.locale = Locale(identifier: "en_US")
        return f.string(from: then)
    }

    static func computeUnread(messages: [StoredMessage], ownUserId: Data, readThrough: UInt64) -> Int {
        messages.filter {
            isVisibleChatKind($0.kind)
                && $0.senderUserId != ownUserId
                && $0.lamport > readThrough
        }.count
    }

    static func lastVisibleMessage(_ messages: [StoredMessage]) -> StoredMessage? {
        messages.filter { isVisibleChatKind($0.kind) }.max(by: { $0.timestamp < $1.timestamp })
    }

    static func previewText(_ message: StoredMessage) -> String {
        if message.kind == ProtocolKind.attachmentManifest {
            return AttachmentPayload.previewLabel(AttachmentPayload.decode(message.payload))
        }
        return String(data: message.payload, encoding: .utf8) ?? ""
    }
}
