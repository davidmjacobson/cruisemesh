import Foundation

enum ChatReadMarker {
    /// Records this device's READ watermark for every sender stream represented
    /// by a chat-list row. Direct chats have one peer stream; groups have one
    /// stream per other member, matching Android's chat-list action.
    @discardableResult
    static func markRead(
        store: MessageStore,
        ownUserId: Data,
        chatId: Data,
        isGroup: Bool
    ) -> Int {
        let senderIds: [Data]
        if isGroup, let group = try? store.getGroup(groupId: chatId) {
            senderIds = group.memberUserIds.filter { $0 != ownUserId }
        } else {
            senderIds = [chatId]
        }

        var recorded = 0
        for senderId in senderIds {
            let through = PeerStreamWatermark.through(
                store: store,
                chatId: chatId,
                senderUserId: senderId
            )
            guard through > 0 else { continue }
            do {
                try store.recordOutgoingReceipt(
                    chatId: chatId,
                    senderUserId: senderId,
                    receiptType: ReceiptType.read,
                    throughLamport: through
                )
                recorded += 1
            } catch {
                continue
            }
        }
        return recorded
    }
}
