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

    func encode() -> Data {
        var out = Data()
        out.append(Self.wireVersion)
        writeBytes16(target.senderUserId, to: &out)
        out.append(contentsOf: target.lamport.bigEndianBytes)
        out.append(target.kind)
        writeUtf16(emoji, to: &out)
        return out
    }

    static func decode(_ data: Data) -> ReactionPayload? {
        var cursor = DataCursor(data)
        guard cursor.readUInt8() == wireVersion,
              let senderUserId = cursor.readBytes16(),
              let lamport = cursor.readUInt64(),
              let kind = cursor.readUInt8(),
              let emoji = cursor.readUtf16(maxBytes: maxEmojiBytes),
              cursor.isAtEnd,
              emoji.isEmpty || Data(emoji.utf8).count <= maxEmojiBytes else {
            return nil
        }
        return ReactionPayload(
            target: MessageTarget(senderUserId: senderUserId, lamport: lamport, kind: kind),
            emoji: emoji
        )
    }

    private static let wireVersion: UInt8 = 1
    private static let maxEmojiBytes = 32
}

struct ReactionSummary: Equatable {
    let emoji: String
    let count: Int
    let reactedByOwnUser: Bool
}

func reactionSummariesByTarget(messages: [StoredMessage], ownUserId: Data) -> [String: [ReactionSummary]] {
    var byTarget: [String: [String: ReactionState]] = [:]
    for message in messages where message.kind == ProtocolKind.reaction {
        guard let reaction = ReactionPayload.decode(message.payload) else { continue }
        let reactorKey = UserIdHex.encode(message.senderUserId)
        var reactions = byTarget[reaction.target.stableKey] ?? [:]
        if let previous = reactions[reactorKey], previous.lamport > message.lamport {
            continue
        }
        if reaction.emoji.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            reactions.removeValue(forKey: reactorKey)
        } else {
            reactions[reactorKey] = ReactionState(
                lamport: message.lamport,
                emoji: reaction.emoji,
                reactedByOwnUser: message.senderUserId == ownUserId
            )
        }
        byTarget[reaction.target.stableKey] = reactions
    }
    return byTarget.mapValues { reactions in
        Dictionary(grouping: reactions.values, by: { $0.emoji })
            .map { emoji, entries in
                ReactionSummary(
                    emoji: emoji,
                    count: entries.count,
                    reactedByOwnUser: entries.contains { $0.reactedByOwnUser }
                )
            }
            .sorted {
                if $0.reactedByOwnUser != $1.reactedByOwnUser {
                    return $0.reactedByOwnUser
                }
                return $0.emoji < $1.emoji
            }
    }
}

private struct ReactionState {
    let lamport: UInt64
    let emoji: String
    let reactedByOwnUser: Bool
}

private func writeBytes16(_ bytes: Data, to out: inout Data) {
    out.append(contentsOf: UInt16(bytes.count).bigEndianBytes)
    out.append(bytes)
}

private func writeUtf16(_ value: String, to out: inout Data) {
    let bytes = Data(value.utf8)
    out.append(contentsOf: UInt16(bytes.count).bigEndianBytes)
    out.append(bytes)
}

private struct DataCursor {
    private let data: Data
    private var offset = 0

    init(_ data: Data) {
        self.data = data
    }

    var isAtEnd: Bool { offset == data.count }

    mutating func readUInt8() -> UInt8? {
        guard offset < data.count else { return nil }
        defer { offset += 1 }
        return data[offset]
    }

    mutating func readUInt16() -> UInt16? {
        guard let bytes = read(count: 2) else { return nil }
        return bytes.reduce(UInt16(0)) { ($0 << 8) | UInt16($1) }
    }

    mutating func readUInt64() -> UInt64? {
        guard let bytes = read(count: 8) else { return nil }
        return bytes.reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
    }

    mutating func readBytes16() -> Data? {
        guard let len = readUInt16() else { return nil }
        return read(count: Int(len)).map { Data($0) }
    }

    mutating func readUtf16(maxBytes: Int) -> String? {
        guard let len = readUInt16(), len <= maxBytes, let bytes = read(count: Int(len)) else {
            return nil
        }
        return String(data: Data(bytes), encoding: .utf8)
    }

    private mutating func read(count: Int) -> [UInt8]? {
        guard count >= 0, offset + count <= data.count else { return nil }
        defer { offset += count }
        return Array(data[offset..<(offset + count)])
    }
}

private extension FixedWidthInteger {
    var bigEndianBytes: [UInt8] {
        withUnsafeBytes(of: self.bigEndian) { Array($0) }
    }
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
