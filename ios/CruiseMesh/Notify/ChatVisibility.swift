import Foundation

/**
 Process-wide record of which 1:1 chat is currently on screen, if any.
 Android `ChatVisibility` parity: set on appear, clear on disappear with a
 content-matched clear so navigation transitions cannot wipe a newer chat.
 */
enum ChatVisibility {
    private static let lock = NSLock()
    private static var visibleChatId: Data?

    /** Marks `chatId`'s chat as the one currently on screen. */
    static func setVisible(_ chatId: Data) {
        lock.lock(); defer { lock.unlock() }
        visibleChatId = chatId
    }

    /**
     Clears the visible chat, but only if it is still `chatId`. During a
     navigation transition the incoming chat's `setVisible` can run before
     the outgoing chat's `clearVisible`, and an unconditional clear would
     wipe out the newer registration.
     */
    static func clearVisible(_ chatId: Data) {
        lock.lock(); defer { lock.unlock() }
        if visibleChatId == chatId {
            visibleChatId = nil
        }
    }

    /** True if `chatId`'s chat is currently on screen (value equality on Data). */
    static func isVisible(_ chatId: Data) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return visibleChatId == chatId
    }

    /** Test-only: forget any registration so tests don't leak state. */
    static func reset() {
        lock.lock(); defer { lock.unlock() }
        visibleChatId = nil
    }
}
