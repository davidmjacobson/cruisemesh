import Foundation

/// Decides whether saving a draft over its previous value should announce a
/// chat-changed event (XP1). The chat list only needs to know whether a
/// draft *exists* -- it re-reads the live text on its own reload passes --
/// so only the empty <-> non-empty transition needs to be announced.
/// Announcing every keystroke turned typing into a full chat-list reload
/// plus, before this fix, a read receipt + relay sync kick per character.
/// Mirrors Android `DraftChangeSignal`.
enum DraftChangeSignal {
    static func shouldNotify(previous: String, next: String) -> Bool {
        isEmpty(previous) != isEmpty(next)
    }

    // Mirrors DraftStore.save's own emptiness rule.
    private static func isEmpty(_ text: String) -> Bool {
        text.trimmingCharacters(in: .newlines).isEmpty
    }
}
