import Foundation

/// What one tap of send did, and therefore what the screen owes the user.
enum ComposerSendStatus: Equatable {
    /// An empty composer: nothing was attempted and nothing changed.
    case nothingToSend
    /// The message reached the durable local message/outbound transaction.
    case queued
    /// It did not. Every character the user typed is still in the composer.
    case notQueued
}

/// The composer's contents *after* a send attempt, plus `status`.
///
/// The screen assigns `draft` and `pendingPhoto` back unconditionally, so the
/// only code that can ever empty the composer is `ComposerSendPolicy.attempt`
/// — and it only does so for `.queued`.
struct ComposerSendOutcome {
    let draft: String
    let pendingPhoto: Data?
    let status: ComposerSendStatus
}

/// The one rule the composer follows when send is tapped: the typed text
/// survives unless the message was durably queued.
///
/// This used to live inline in `ChatView` and `GroupChatView` as
/// `sender.sendText(...)` immediately followed by `draft = ""`, with the
/// sender returning `Void` — so a send that stored nothing still emptied the
/// field, and the user watched their message disappear with nothing said.
/// Extracted here as a plain type with no SwiftUI so the rule is unit-testable
/// rather than reachable only through a live view and a real mesh transport.
/// Android carries the same class, name for name.
///
/// Failure is preserved verbatim, not re-trimmed: the draft handed back is the
/// exact string the user had, trailing newline and all, so retrying costs a tap
/// rather than retyping.
enum ComposerSendPolicy {
    /// Attempts one send of the current composer contents.
    ///
    /// A staged photo wins over bare text — the trimmed draft rides along as
    /// its caption, which is how a single tap sends "photo + words" as one
    /// attachment rather than two messages.
    static func attempt(
        draft: String,
        pendingPhoto: Data?,
        sendPhoto: (Data, String) -> SendResult,
        sendText: (String) -> SendResult
    ) -> ComposerSendOutcome {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let result: SendResult
        if let photo = pendingPhoto {
            result = sendPhoto(photo, text)
        } else if !text.isEmpty {
            result = sendText(text)
        } else {
            return ComposerSendOutcome(draft: draft, pendingPhoto: pendingPhoto, status: .nothingToSend)
        }
        switch result {
        case .stored:
            return ComposerSendOutcome(draft: "", pendingPhoto: nil, status: .queued)
        case .failed:
            return ComposerSendOutcome(draft: draft, pendingPhoto: pendingPhoto, status: .notQueued)
        }
    }
}
