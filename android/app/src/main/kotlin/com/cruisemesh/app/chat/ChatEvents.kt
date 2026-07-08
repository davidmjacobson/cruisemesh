package com.cruisemesh.app.chat

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow

/**
 * Process-wide singleton (same lazy/eager-object pattern as
 * [com.cruisemesh.app.AppStore] and [com.cruisemesh.app.mesh.MeshRouter])
 * that replaces [ChatScreen]'s interim 1s polling loop with a real
 * store-changed push: whoever mutates a chat's messages calls
 * [notifyChatChanged], and [ChatScreen] collects [changes] to know when to
 * reload from the store.
 *
 * [notifyChatChanged] is called from very different threads --
 * [com.cruisemesh.app.mesh.MeshService.handleIncomingText] runs on a BLE
 * binder thread, while [RealMeshSender.sendText] runs on the UI thread -- so
 * this wraps a [MutableSharedFlow] with extra buffer capacity and emits via
 * [MutableSharedFlow.tryEmit], which is non-suspending and thread-safe: it
 * never blocks a binder thread, and (short of an absurd notification burst
 * exceeding the buffer) never silently drops an event either.
 */
object ChatEvents {
    private val _changes = MutableSharedFlow<ByteArray>(extraBufferCapacity = 16)

    /** Emits the `chatId` (DESIGN.md §7.1) of every chat whose stored messages just changed. */
    val changes: SharedFlow<ByteArray> = _changes.asSharedFlow()

    /** Announces that [chatId]'s stored messages changed. Safe to call from any thread. */
    fun notifyChatChanged(chatId: ByteArray) {
        _changes.tryEmit(chatId)
    }
}
