import Combine
import Foundation

/// Delivers `body` to subscribers on the main thread.
///
/// Every subscriber of the subjects below is a SwiftUI view, and two of them
/// (`ChatListView`, `ChatView`) `sink` without a `receive(on:)` of their own,
/// so whatever thread sends is the thread that reloads their state. That was
/// always the main one while the mesh pipeline lived on the main actor; it
/// runs on `MeshController`'s serial mesh queue now, so the hop belongs here
/// rather than at each of the five subscribe sites -- one place to be right,
/// and no call site can forget.
///
/// Sends already on the main thread stay synchronous, so a UI-driven change
/// still reloads within the same run loop turn exactly as before.
private func onMainThread(_ body: @escaping @Sendable () -> Void) {
    if Thread.isMainThread {
        body()
    } else {
        DispatchQueue.main.async(execute: body)
    }
}

/// Push notifications that a chat's local state changed (new message or receipt).
/// Mirrors Android `ChatEvents`.
enum ChatEvents {
    static let subject = PassthroughSubject<Data, Never>()

    static func notifyChatChanged(_ chatId: Data) {
        onMainThread { subject.send(chatId) }
    }
}

enum RelaySyncEvents {
    static let subject = PassthroughSubject<Void, Never>()

    static func requestSync() {
        onMainThread { subject.send(()) }
    }
}

struct FriendImportEvent {
    let contact: Contact
    let directBluetooth: Bool
}

enum FriendImportEvents {
    static let subject = PassthroughSubject<FriendImportEvent, Never>()

    static func notify(_ event: FriendImportEvent) {
        onMainThread { subject.send(event) }
    }
}
