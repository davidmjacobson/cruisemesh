package com.cruisemesh.app.mesh

/**
 * In-process "relay queue changed" signal. Used to nudge [MeshService] when a
 * newly queued outbound or family-carried envelope is worth uploading to the
 * internet relay immediately if connectivity is already available.
 */
object RelaySyncEvents {
    @Volatile private var handler: (() -> Unit)? = null

    fun register(handler: () -> Unit) {
        this.handler = handler
    }

    fun unregister() {
        handler = null
    }

    fun requestSync() {
        handler?.invoke()
    }
}
