package com.cruisemesh.app.notify

import androidx.annotation.VisibleForTesting

/**
 * Process-wide record of which 1:1 chat is currently on screen, if any. A
 * chat is keyed the same way it is everywhere else locally: by the OTHER
 * party's userId bytes (see the wire-vs-local `chatId` discussion on
 * [com.cruisemesh.app.mesh.MeshService]).
 *
 * Registered/unregistered by the chat route in
 * [com.cruisemesh.app.MainActivity] via a lifecycle-aware `DisposableEffect`
 * keyed on the chat destination's lifecycle: set on `ON_START` (the chat is
 * actually on screen -- its nav destination is resumed AND the app is
 * foregrounded) and cleared on `ON_STOP` (navigated away OR app backgrounded).
 * This is deliberately NOT plain composition tracking: the NavHost composition
 * survives backgrounding, so a composition-only clear would keep a chat marked
 * "visible" while the user is in another app -- suppressing its notifications
 * (and sending false read receipts) until the process was fully destroyed.
 *
 * Two consumers, both outside this class (this class only *answers*
 * "is chat X on screen?" -- it never decides what to do about it):
 *
 * 1. **Notification suppression.** When a new incoming message is stored,
 *    the mesh layer asks [isVisible] before calling
 *    [MessageNotifier.notifyIncomingMessage] -- no notification for the
 *    chat the user is already looking at. Note the flip side: any newly
 *    stored incoming message whose chat is NOT visible will notify,
 *    including older messages arriving in a burst via reconnect catch-up.
 *    That's acceptable for now; smarter batching/age cutoffs are a later
 *    refinement.
 * 2. **Read receipts.** "This chat is on screen" is exactly the signal for
 *    when messages are actually *seen* -- [com.cruisemesh.app.mesh.MeshService.handleIncomingText]
 *    checks [isVisible] to send a read (not just delivered) receipt for a
 *    message arriving while its chat is already open, and
 *    [com.cruisemesh.app.mesh.ChatViewEvents] (fired from the same
 *    `DisposableEffect` that calls [setVisible]) sends a read receipt for
 *    whatever's already stored when a chat *becomes* visible (DESIGN.md
 *    §7.2).
 *
 * Thread-safe: composition writes happen on the main thread but mesh-side
 * reads come from BLE callback threads.
 */
object ChatVisibility {
    private val lock = Any()
    private var visible: ByteArray? = null

    /** Marks [chatId]'s chat as the one currently on screen. */
    fun setVisible(chatId: ByteArray) {
        synchronized(lock) { visible = chatId.copyOf() }
    }

    /**
     * Clears the visible chat, but only if it is still [chatId]. During a
     * navigation transition the incoming chat's `setVisible` can run before
     * the outgoing chat's `clearVisible` (composition of the two screens
     * overlaps), and an unconditional clear would wipe out the newer
     * registration.
     */
    fun clearVisible(chatId: ByteArray) {
        synchronized(lock) {
            if (visible?.contentEquals(chatId) == true) visible = null
        }
    }

    /**
     * True if [chatId]'s chat is currently on screen. Compares by content
     * ([ByteArray.contentEquals]) -- `==` on ByteArray is referential in
     * Kotlin, and callers pass ids from different sources (nav args, wire
     * frames, the store).
     */
    fun isVisible(chatId: ByteArray): Boolean =
        synchronized(lock) { visible?.contentEquals(chatId) == true }

    /** Test-only: forget any registration so tests don't leak state into each other. */
    @VisibleForTesting
    internal fun reset() {
        synchronized(lock) { visible = null }
    }
}
