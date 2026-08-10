import Foundation

enum ConversationScrollDecision: Equatable {
    case none
    case autoScroll
    case showNewMessages(targetRowId: String?)
}

/// Shared direct/group conversation policy matching Android's
/// `ChatScrollLogic`: history backfill never moves the reader, an incoming
/// tail message only auto-scrolls when the reader is already near the bottom,
/// and a delayed message inserted above the tail gets an explicit jump target.
enum ConversationScrollPolicy {
    static func decide(
        previousRowIds: [String],
        currentRowIds: [String],
        lateArrivalRowIds: Set<String>,
        isNearBottom: Bool,
        newestIsOwnMessage: Bool
    ) -> ConversationScrollDecision {
        guard let currentNewest = currentRowIds.last else { return .none }
        guard !previousRowIds.isEmpty else { return .autoScroll }

        let previousSet = Set(previousRowIds)
        let insertedAboveTarget = currentRowIds.first {
            !previousSet.contains($0) && lateArrivalRowIds.contains($0)
        }
        let newestChanged = previousRowIds.last != currentNewest

        if !newestChanged {
            return insertedAboveTarget.map {
                .showNewMessages(targetRowId: $0)
            } ?? .none
        }

        if isNearBottom || newestIsOwnMessage {
            return .autoScroll
        }
        return .showNewMessages(targetRowId: nil)
    }
}
