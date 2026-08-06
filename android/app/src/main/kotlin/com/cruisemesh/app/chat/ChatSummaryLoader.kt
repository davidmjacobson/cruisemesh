package com.cruisemesh.app.chat

import android.content.Context
import com.cruisemesh.app.notify.ChatMuteStore
import com.cruisemesh.app.ui.ChatSummary
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreContactDisplayName

/**
 * Builds the home [ChatSummary] snapshot off the main thread using the core
 * [MessageStore.chatPreview] path (G1) — never marshals full chat histories.
 */
object ChatSummaryLoader {
    fun loadAll(
        context: Context,
        store: MessageStore,
        identity: Identity,
    ): List<ChatSummary> {
        val contacts = store.listContacts()
        val direct = contacts.map { c ->
            val preview = store.chatPreview(c.userId, identity.userId)
            ChatSummary(
                chatId = c.userId,
                title = coreContactDisplayName(c),
                isGroup = false,
                contact = c,
                lastMessage = preview.lastMessage,
                unreadCount = preview.unreadCount.toInt(),
                ownDeliveredThrough = preview.ownDeliveredThrough,
                ownReadThrough = preview.ownReadThrough,
                avatarBytes = preview.avatarBytes,
                draft = DraftStore.load(context, c.userId),
                isMuted = ChatMuteStore.isMuted(context, c.userId),
            )
        }
        val groups = store.listGroups().map { g ->
            val preview = store.chatPreview(g.id, identity.userId)
            ChatSummary(
                chatId = g.id,
                title = g.name,
                isGroup = true,
                group = g,
                lastMessage = preview.lastMessage,
                unreadCount = preview.unreadCount.toInt(),
                ownDeliveredThrough = 0uL,
                ownReadThrough = 0uL,
                draft = DraftStore.load(context, g.id),
                isMuted = ChatMuteStore.isMuted(context, g.id),
            )
        }
        return (direct + groups).sortedByDescending { it.lastMessage?.timestamp ?: 0L }
    }
}
