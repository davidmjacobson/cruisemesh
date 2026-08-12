import SwiftUI
import UIKit

/// The long-press menu for a chat bubble, shared by 1:1 and group threads.
///
/// The six reactions sit in ONE horizontal row above Reply / Save / Copy / Info,
/// mirroring Android's `MessageFocusOverlay` (a reaction bar floating above the
/// bubble, an action panel below it) so the two platforms read as the same
/// product side by side. The emoji order is `reactionChoices`, which matches
/// Android's `REACTION_CHOICES` exactly.
///
/// Before this, each emoji was its own full-width menu row: nine stacked rows
/// that swallowed most of an iPad screen.
struct MessageActionsMenu: View {
    let canReply: Bool
    /// Empty when there is nothing to put on the pasteboard (a photo or voice
    /// memo with no caption), which hides Copy the way Android's `canCopy` does.
    let copyText: String
    /// Present only for image attachments. Signal makes Save contextual rather
    /// than showing a disabled row for text, audio, or malformed attachments.
    let imageData: Data?
    /// The reaction this device already sent for this message, if any, so
    /// VoiceOver can say the tap would remove it.
    let ownReaction: String?
    let onReact: (String) -> Void
    let onReply: () -> Void
    let onCopy: () -> Void
    let onStatus: (String) -> Void
    let onInfo: () -> Void

    var body: some View {
        // A ControlGroup in a menu lays its buttons out as a palette: one
        // horizontal row, sized to its contents, so a wide iPad menu does not
        // stretch the emoji across the screen. Palette style landed in iOS 17;
        // on iOS 16 the six choices collapse into a "React" submenu instead —
        // still one row in the menu, never six.
        if #available(iOS 17.0, *) {
            ControlGroup {
                reactionButtons
            }
            .controlGroupStyle(.palette)
        } else {
            Menu {
                reactionButtons
            } label: {
                Label("React", systemImage: "face.smiling")
            }
        }
        if canReply {
            Button(action: onReply) {
                Label("Reply", systemImage: "arrowshape.turn.up.left")
            }
        }
        if let imageData {
            Button {
                ImageGallery.saveJpeg(imageData) { result in
                    switch result {
                    case .saved:
                        onStatus("Saved to Photos")
                    case .denied:
                        onStatus("Photo Library access is required to save images. Enable it in Settings.")
                    case .failed(let message):
                        onStatus(message)
                    }
                }
            } label: {
                Label("Save image", systemImage: "square.and.arrow.down")
            }
        }
        if !copyText.isEmpty {
            Button(action: onCopy) {
                Label("Copy", systemImage: "doc.on.doc")
            }
        }
        Button(action: onInfo) {
            Label("Info", systemImage: "info.circle")
        }
    }

    /// Menu buttons carry the system's own metrics, so each emoji keeps a
    /// full-height (>= 44pt) hit target whether it is drawn in the palette row
    /// or in the iOS 16 submenu.
    @ViewBuilder
    private var reactionButtons: some View {
        ForEach(reactionChoices, id: \.self) { emoji in
            Button {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                onReact(emoji)
            } label: {
                Text(emoji)
            }
            .accessibilityLabel(reactionAccessibilityLabel(emoji))
        }
    }

    private func reactionAccessibilityLabel(_ emoji: String) -> String {
        emoji == ownReaction
            ? String(localized: "React \(emoji), selected. Tap to remove")
            : String(localized: "React \(emoji)")
    }
}

/// What Copy puts on the pasteboard: the text of a message, or an attachment's
/// caption. Empty means there is nothing to copy.
func messageCopyText(_ message: StoredMessage) -> String {
    if message.kind == ProtocolKind.attachmentManifest {
        return AttachmentPayload.decode(message.payload)?.caption ?? ""
    }
    return String(data: message.payload, encoding: .utf8) ?? ""
}

/// The image payload exposed by the contextual Save action. Keeping the rule
/// beside `messageCopyText` prevents 1:1 and group menus from drifting.
func messageImageData(_ message: StoredMessage) -> Data? {
    guard message.kind == ProtocolKind.attachmentManifest,
          let attachment = AttachmentPayload.decode(message.payload),
          attachment.mediaType == .image else { return nil }
    return attachment.blob
}
