package com.cruisemesh.app.mesh

/**
 * Process-wide singleton (same registration pattern as [MeshRouter]: a
 * handler function registered on [MeshService] start and unregistered on
 * stop) that lets the chat UI announce "the user just opened this chat"
 * without depending on [MeshService] directly -- mirroring how
 * [com.cruisemesh.app.chat.MeshSender] never depends on a concrete
 * transport.
 *
 * The only consumer today is the read-receipt-on-view flow (DESIGN.md
 * §7.2): [com.cruisemesh.app.MainActivity]'s `ChatRoute` calls
 * [onChatViewed] from the same `DisposableEffect` that calls
 * [com.cruisemesh.app.notify.ChatVisibility.setVisible], and
 * [MeshService] registers a handler that sends a READ receipt for
 * whatever is currently stored from that chat's peer.
 *
 * If nothing is registered (mesh not started, or started and stopped
 * again) [onChatViewed] is simply a no-op -- there is no live link to send
 * a receipt over anyway, and the peer's next real sync (their next HELLO)
 * will pick up the read state once the mesh is running again.
 */
object ChatViewEvents {
    @Volatile private var handler: ((ByteArray) -> Unit)? = null

    /** [MeshService] calls this when it starts. */
    fun register(handler: (ByteArray) -> Unit) {
        this.handler = handler
    }

    /** [MeshService] calls this when it stops, so a stale handler is never invoked. */
    fun unregister() {
        handler = null
    }

    /** [chatId]'s chat (the OTHER party's userId, as everywhere locally) was just opened on screen. */
    fun onChatViewed(chatId: ByteArray) {
        handler?.invoke(chatId)
    }
}
