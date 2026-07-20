import Foundation
import SwiftUI

let reactionChoices = ["👍", "❤️", "😂", "😮", "😢", "🙏"]

struct MessageTarget: Hashable {
    let senderUserId: Data
    let lamport: UInt64
    let kind: UInt8

    var stableKey: String {
        "\(UserIdHex.encode(senderUserId)):\(lamport):\(kind)"
    }
}

struct ReactionPayload: Equatable {
    let target: MessageTarget
    let emoji: String

    /// `nil` on a (never-observed-in-practice) encode failure — callers
    /// no-op rather than crash (FI9).
    func encode() -> Data? {
        try? encodeReactionPayload(payload: CoreReactionPayload(
            target: CoreMessageTarget(
                senderUserId: target.senderUserId,
                lamport: target.lamport,
                kind: target.kind
            ),
            emoji: emoji
        ))
    }

    static func decode(_ data: Data) -> ReactionPayload? {
        guard let decoded = decodeReactionPayload(bytes: data) else { return nil }
        return ReactionPayload(
            target: MessageTarget(
                senderUserId: decoded.target.senderUserId,
                lamport: decoded.target.lamport,
                kind: decoded.target.kind
            ),
            emoji: decoded.emoji
        )
    }

}

struct ReactionSummary: Equatable {
    let emoji: String
    let count: Int
    let reactedByOwnUser: Bool
}

func reactionSummariesByTarget(messages: [StoredMessage], ownUserId: Data) -> [String: [ReactionSummary]] {
    Dictionary(uniqueKeysWithValues: coreReactionSummariesByTarget(
        messages: messages, ownUserId: ownUserId
    ).map { summary in
        let target = MessageTarget(senderUserId: summary.target.senderUserId,
                                   lamport: summary.target.lamport, kind: summary.target.kind)
        return (target.stableKey, summary.reactions.map {
            ReactionSummary(emoji: $0.emoji, count: Int($0.count), reactedByOwnUser: $0.reactedByOwnUser)
        })
    })
}

struct ReactionPillRow: View {
    let reactions: [ReactionSummary]
    let isOwn: Bool
    let onReact: (String) -> Void

    var body: some View {
        HStack(spacing: 4) {
            ForEach(reactions, id: \.emoji) { reaction in
                Button {
                    onReact(reaction.emoji)
                } label: {
                    Text(reaction.count > 1 ? "\(reaction.emoji) \(reaction.count)" : reaction.emoji)
                        .font(.caption)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 3)
                        .background(
                            Capsule()
                                .fill(reaction.reactedByOwnUser ? Color.accentColor.opacity(0.22) : Color.secondary.opacity(0.16))
                        )
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.leading, isOwn ? 0 : 10)
        .padding(.trailing, isOwn ? 10 : 0)
    }
}
