import Foundation

enum DigestSync {
    static func isExpectedChatId(digestChatId: Data, helloUserId: Data?) -> Bool {
        guard let helloUserId else { return false }
        return digestChatId == helloUserId
    }

    static func throughLamportForSender(entries: [DigestEntry], senderUserId: Data) -> UInt64 {
        entries.first(where: { Data($0.senderUserId) == senderUserId })?.throughLamport ?? 0
    }

    static func throughLamportForSelf(entries: [DigestEntry], ownUserId: Data) -> UInt64 {
        throughLamportForSender(entries: entries, senderUserId: ownUserId)
    }
}
