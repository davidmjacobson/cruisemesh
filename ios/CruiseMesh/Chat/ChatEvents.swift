import Combine
import Foundation

/// Push notifications that a chat's local state changed (new message or receipt).
/// Mirrors Android `ChatEvents`.
enum ChatEvents {
    static let subject = PassthroughSubject<Data, Never>()

    static func notifyChatChanged(_ chatId: Data) {
        subject.send(chatId)
    }
}

enum RelaySyncEvents {
    static let subject = PassthroughSubject<Void, Never>()

    static func requestSync() {
        subject.send(())
    }
}
