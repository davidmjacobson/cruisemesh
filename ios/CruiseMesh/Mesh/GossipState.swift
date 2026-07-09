import Foundation

/// Process-wide seen-ID set for flood dedupe (Android `GossipState`).
enum GossipState {
    static let seenIds = SeenIds()
}
