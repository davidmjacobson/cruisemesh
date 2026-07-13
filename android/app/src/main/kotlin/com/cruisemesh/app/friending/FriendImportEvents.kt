package com.cruisemesh.app.friending

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import uniffi.cruisemesh_core.Contact

data class FriendImportEvent(val contact: Contact, val directBle: Boolean)

/** Process-local signal for a newly imported, authenticated mutual friend card. */
object FriendImportEvents {
    private val events = MutableSharedFlow<FriendImportEvent>(extraBufferCapacity = 8)
    val imports = events.asSharedFlow()

    fun notifyImported(contact: Contact, directBle: Boolean) {
        events.tryEmit(FriendImportEvent(contact, directBle))
    }
}
