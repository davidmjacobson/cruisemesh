import Foundation

enum ChatVisibility {
    private static let lock = NSLock()
    private static var visibleChatId: Data?

    static func setVisible(_ chatId: Data?) {
        lock.lock(); defer { lock.unlock() }
        visibleChatId = chatId
    }

    static func isVisible(_ chatId: Data) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return visibleChatId == chatId
    }
}
