import Foundation

enum ChatMuteStore {
    private static let prefix = "cruisemesh.chat.muted."
    static func isMuted(_ chatId: Data) -> Bool { UserDefaults.standard.bool(forKey: key(chatId)) }
    static func setMuted(_ muted: Bool, chatId: Data) { UserDefaults.standard.set(muted, forKey: key(chatId)) }
    private static func key(_ chatId: Data) -> String { prefix + chatId.base64EncodedString() }
}
