import Foundation
import SwiftUI
import UIKit

private let originalMessageUnavailable = "Original message unavailable"

struct MessageReplyMetadata {
    let msgId: Data?
    let quoted: QuotedMessagePreview?
}

struct QuotedMessagePreview {
    let senderLabel: String?
    let text: String
    let target: StoredMessage?
}

func replyMessageKey(_ message: StoredMessage) -> String {
    let sender = message.senderUserId.map { String(format: "%02x", $0) }.joined()
    return "\(sender):\(message.lamport):\(message.kind)"
}

func loadMessageReplyMetadata(
    store: MessageStore,
    messages: [StoredMessage],
    senderLabelFor: (StoredMessage) -> String
) -> [String: MessageReplyMetadata] {
    var result: [String: MessageReplyMetadata] = [:]
    for metadata in (try? store.replyMetadata(messages: messages)) ?? [] {
        let message = messages.first {
            $0.senderUserId == metadata.message.senderUserId && $0.lamport == metadata.message.lamport
                && $0.kind == metadata.message.kind
        }!
        let quoted = metadata.replyToMsgId.map { _ in
            quotedMessagePreview(
                target: metadata.target,
                senderLabelFor: senderLabelFor
            )
        }
        result[replyMessageKey(message)] = MessageReplyMetadata(
            msgId: metadata.msgId,
            quoted: quoted
        )
    }
    return result
}

func quotedMessagePreview(
    target: StoredMessage?,
    senderLabelFor: (StoredMessage) -> String
) -> QuotedMessagePreview {
    guard let target else {
        return QuotedMessagePreview(
            senderLabel: nil,
            text: originalMessageUnavailable,
            target: nil
        )
    }
    return QuotedMessagePreview(
        senderLabel: senderLabelFor(target),
        text: quotedMessageText(target),
        target: target
    )
}

func quotedMessageText(_ message: StoredMessage) -> String {
    if message.kind == ProtocolKind.attachmentManifest {
        return AttachmentPayload.previewLabel(AttachmentPayload.decode(message.payload))
    }
    let text = (String(data: message.payload, encoding: .utf8) ?? "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    return text.isEmpty ? "Message" : text
}

struct QuotedMessageBlock: View {
    let preview: QuotedMessagePreview
    let accentColor: Color
    let contentColor: Color
    var onTap: (() -> Void)?

    var body: some View {
        HStack(spacing: 8) {
            RoundedRectangle(cornerRadius: 2)
                .fill(accentColor)
                .frame(width: 4, height: 42)
            VStack(alignment: .leading, spacing: 2) {
                if let senderLabel = preview.senderLabel {
                    Text(senderLabel)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(accentColor)
                        .lineLimit(1)
                }
                Text(preview.text)
                    .font(.caption)
                    .foregroundStyle(contentColor.opacity(0.86))
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(contentColor.opacity(0.1))
        )
        .contentShape(Rectangle())
        .onTapGesture { onTap?() }
    }
}

struct ReplyComposerPreview: View {
    let preview: QuotedMessagePreview
    let onCancel: () -> Void

    var body: some View {
        HStack(spacing: 4) {
            QuotedMessageBlock(
                preview: preview,
                accentColor: .accentColor,
                contentColor: .primary,
                onTap: nil
            )
            Button(action: onCancel) {
                Image(systemName: "xmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }
            .accessibilityLabel("Cancel reply")
        }
        .padding(6)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color(uiColor: .secondarySystemBackground))
        )
    }
}
