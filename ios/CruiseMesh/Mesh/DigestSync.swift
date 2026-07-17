import Foundation

enum DigestSync {
    static func isExpectedChatId(digestChatId: Data, helloUserId: Data?) -> Bool {
        digestIsExpectedChatId(digestChatId: digestChatId, helloUserId: helloUserId)
    }

    static func throughLamportForSender(entries: [DigestEntry], senderUserId: Data) -> UInt64 {
        digestThroughLamportForSender(entries: entries, senderUserId: senderUserId)
    }

    static func throughLamportForSelf(entries: [DigestEntry], ownUserId: Data) -> UInt64 {
        throughLamportForSender(entries: entries, senderUserId: ownUserId)
    }
}
