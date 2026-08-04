import Foundation

/// Which messages should show when they reached this phone, and at what time.
///
/// A bubble carries the sender's send time, so a message carried for hours is
/// spliced into the middle of the thread -- above replies sent while it was in
/// flight. The core decides which of those are confusing enough to annotate
/// (`core/src/late_arrival.rs`: displaced by something already here, and at
/// least ten minutes behind its send time); this maps the answer onto the row
/// ids the chat views look bubbles up by.
///
/// Returns arrival times only for the flagged messages -- absent means "render
/// nothing", which is the overwhelmingly common case.
func lateArrivalTimesByKey(
    visibleMessages: [StoredMessage],
    receivedTimes: [CoreMessageReceivedAt],
    ownUserId: Data
) -> [String: Int64] {
    guard !visibleMessages.isEmpty else { return [:] }
    var receivedByKey: [String: Int64] = [:]
    for row in receivedTimes {
        receivedByKey[arrivalKey(row.senderUserId, row.lamport)] = row.receivedAtMs
    }
    let inputs = visibleMessages.map { message in
        LateArrivalInput(
            displayTsMs: message.timestamp,
            arrivalTsMs: receivedByKey[arrivalKey(message.senderUserId, message.lamport)],
            isOwn: message.senderUserId == ownUserId
        )
    }
    let flags = coreLateArrivalFlags(rows: inputs)
    var result: [String: Int64] = [:]
    for (index, message) in visibleMessages.enumerated() {
        guard index < flags.count, flags[index], let arrival = inputs[index].arrivalTsMs else { continue }
        result[lateArrivalRowKey(message)] = arrival
    }
    return result
}

/// Row identity shared with `ChatRowModel`/`GroupChatRowModel`'s `rowId`.
func lateArrivalRowKey(_ message: StoredMessage) -> String {
    let sender = message.senderUserId.map { String(format: "%02x", $0) }.joined()
    return "\(sender):\(message.lamport):\(message.kind)"
}

/// A message is unique within one chat by (sender, lamport) -- the pair
/// `MessageStore::chat_received_times` keys its rows by. Deliberately without
/// `kind`: the arrival rows don't report one.
private func arrivalKey(_ senderUserId: Data, _ lamport: UInt64) -> String {
    let sender = senderUserId.map { String(format: "%02x", $0) }.joined()
    return "\(sender):\(lamport)"
}

/// Loads the arrival times for one chat, or an empty map if the store errors.
func loadLateArrivalTimes(
    store: MessageStore,
    chatId: Data,
    visibleMessages: [StoredMessage],
    ownUserId: Data
) -> [String: Int64] {
    let received = (try? store.chatReceivedTimes(chatId: chatId)) ?? []
    return lateArrivalTimesByKey(
        visibleMessages: visibleMessages,
        receivedTimes: received,
        ownUserId: ownUserId
    )
}
