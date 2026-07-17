import Foundation

enum DraftStore {
    private static let prefix = "cruisemesh.chat.draft."

    static func load(chatId: Data) -> String {
        UserDefaults.standard.string(forKey: key(chatId)) ?? ""
    }

    static func save(chatId: Data, text: String) {
        let normalized = text.trimmingCharacters(in: .newlines)
        if normalized.isEmpty {
            UserDefaults.standard.removeObject(forKey: key(chatId))
        } else {
            UserDefaults.standard.set(text, forKey: key(chatId))
        }
        ChatEvents.notifyChatChanged(chatId)
    }

    private static func key(_ chatId: Data) -> String {
        prefix + chatId.base64EncodedString()
    }
}
