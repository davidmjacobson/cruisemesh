package com.cruisemesh.app.chat

/**
 * Decides whether saving a draft over its previous value should announce a
 * chat-changed event (XP1). The chat list only needs to know whether a draft
 * *exists* -- it re-reads the live text itself on its own reload passes
 * (navigation back to home, or a real chat-changed event for something else)
 * -- so only the empty <-> non-empty transition needs to be announced.
 * Announcing every keystroke turned typing into a full chat-list reload
 * storm (plus, on iOS, a read receipt + relay sync kick). Kept Android-free
 * so it can be unit-tested directly.
 */
object DraftChangeSignal {
    /** Whether saving [next] over [previous] should notify chat-changed listeners. */
    fun shouldNotify(previous: String, next: String): Boolean = isEmpty(previous) != isEmpty(next)

    // Mirrors DraftStore.save's own emptiness rule.
    private fun isEmpty(text: String): Boolean = text.trim('\n', '\r').isEmpty()
}
