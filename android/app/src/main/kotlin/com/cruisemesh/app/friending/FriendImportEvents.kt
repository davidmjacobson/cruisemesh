package com.cruisemesh.app.friending

import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import uniffi.cruisemesh_core.Contact

data class FriendImportEvent(val contact: Contact, val directBle: Boolean)

internal data class PendingEvent<T>(val id: Long, val value: T)

/**
 * Small process-local queue whose entries survive gaps between collectors.
 *
 * A SharedFlow with no replay drops an event when the QR screen is between
 * composition and collection. Mutual friending commonly completes during that
 * gap, so confirmations must remain pending until the UI explicitly consumes
 * them.
 */
internal class PendingEventQueue<T>(private val capacity: Int = 8) {
    private val nextId = AtomicLong()
    private val mutableEvents = MutableStateFlow<List<PendingEvent<T>>>(emptyList())
    val events: StateFlow<List<PendingEvent<T>>> = mutableEvents.asStateFlow()

    fun enqueue(value: T) {
        val event = PendingEvent(nextId.incrementAndGet(), value)
        mutableEvents.update { current -> (current + event).takeLast(capacity) }
    }

    fun consume(id: Long) {
        mutableEvents.update { current -> current.filterNot { it.id == id } }
    }
}

/** Process-local queue for newly imported, authenticated mutual friend cards. */
object FriendImportEvents {
    private val queue = PendingEventQueue<FriendImportEvent>()
    internal val pendingImports = queue.events

    fun notifyImported(contact: Contact, directBle: Boolean) {
        queue.enqueue(FriendImportEvent(contact, directBle))
    }

    internal fun consume(id: Long) {
        queue.consume(id)
    }
}
