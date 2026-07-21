import Foundation

enum DraftStore {
    private static let prefix = "cruisemesh.chat.draft."

    static func load(chatId: Data) -> String {
        UserDefaults.standard.string(forKey: key(chatId)) ?? ""
    }

    static func save(chatId: Data, text: String) {
        let normalized = text.trimmingCharacters(in: .newlines)
        let previous = load(chatId: chatId)
        if normalized.isEmpty {
            UserDefaults.standard.removeObject(forKey: key(chatId))
        } else {
            UserDefaults.standard.set(text, forKey: key(chatId))
        }
        // XP1: don't fire chat-changed (and, via ChatView's own sink, a
        // reload) on every keystroke -- only the presence transition (a
        // draft appearing/disappearing) needs to be announced.
        if DraftChangeSignal.shouldNotify(previous: previous, next: text) {
            ChatEvents.notifyChatChanged(chatId)
        }
    }

    private static func key(_ chatId: Data) -> String {
        prefix + chatId.base64EncodedString()
    }
}
