package com.cruisemesh.app.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/** `receipt_type` values (DESIGN.md §7.2), for reading own-message tick watermarks out of the store. */
private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u

/**
 * A single 1:1 chat thread (DESIGN.md §7.1: for a 1:1 chat, `chat_id` is
 * simply the peer's UserID). Renders `kind=1` (text) messages oldest-first,
 * auto-scrolled to the newest, with the local user's bubbles right-aligned
 * and the contact's left-aligned (compared via [ByteArray.contentEquals]
 * against `ownUserId`, since [StoredMessage.senderUserId] is raw bytes).
 *
 * Sending goes through [sender] only -- see [MeshSender] for why the UI
 * never talks to a concrete transport directly. The thread (and the two
 * receipt watermarks driving own-message ticks, see below) is reloaded from
 * [store] immediately after a send (for guaranteed instant feedback on the
 * UI thread) and whenever [ChatEvents] reports this chat changed -- which is
 * how a message or receipt [com.cruisemesh.app.mesh.MeshService] receives on
 * a BLE binder thread ends up on screen without a manual refresh or a
 * polling timer.
 *
 * Own messages render a ✓/✓✓ tick (DESIGN.md §7.2), derived per-message from
 * two cumulative watermarks loaded alongside the message list:
 * `receiptThrough(chatId, ownUserId, DELIVERED/READ)`. See [TickStatus] and
 * [tickStatusFor] for the pure derivation and [MessageBubble] for rendering.
 */
@Composable
fun ChatScreen(
    contact: Contact,
    ownUserId: ByteArray,
    sender: MeshSender,
    store: MessageStore,
    onBack: () -> Unit,
) {
    var messages by remember(contact.userId) { mutableStateOf(store.messagesForChat(contact.userId)) }
    var deliveredThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED))
    }
    var readThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ))
    }
    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()

    // Reloads everything this screen renders from the store: the thread
    // itself and the two cumulative receipt watermarks that drive each own
    // bubble's tick (DESIGN.md §7.2). All three are cheap store reads, so
    // there's no reason to reload them independently.
    fun reload() {
        messages = store.messagesForChat(contact.userId)
        deliveredThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED)
        readThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ)
    }

    // Reload whenever anything (MeshService on a BLE binder thread, a sender
    // on the UI thread) reports this chat's store contents changed -- a new
    // message OR a new receipt, both go through the same ChatEvents channel.
    // The collector runs on this composition's main dispatcher, so touching
    // state here is safe; cancellation on dispose is automatic because
    // LaunchedEffect scopes the collection to this composable. chatId is a
    // raw ByteArray, so compare with contentEquals -- == would be
    // referential and never match.
    LaunchedEffect(contact.userId) {
        ChatEvents.changes.collect { changedChatId ->
            if (changedChatId.contentEquals(contact.userId)) {
                reload()
            }
        }
    }

    LaunchedEffect(messages.size) {
        if (messages.isNotEmpty()) {
            listState.animateScrollToItem(messages.size - 1)
        }
    }

    Scaffold { innerPadding ->
        Column(modifier = Modifier.fillMaxSize().padding(innerPadding).padding(horizontal = 16.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(top = 8.dp),
            ) {
                TextButton(onClick = onBack) { Text("Back") }
                Text(
                    contact.name,
                    style = MaterialTheme.typography.titleLarge,
                    modifier = Modifier.padding(start = 8.dp),
                )
            }

            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxWidth().weight(1f).padding(vertical = 8.dp),
            ) {
                items(messages, key = { "${it.senderUserId.contentHashCode()}:${it.lamport}" }) { message ->
                    if (message.kind == KIND_TEXT) {
                        val isOwn = message.senderUserId.contentEquals(ownUserId)
                        MessageBubble(
                            message = message,
                            isOwn = isOwn,
                            // Ticks only ever describe our own messages -- see TickStatus's KDoc.
                            tick = if (isOwn) tickStatusFor(message.lamport, deliveredThrough, readThrough) else null,
                        )
                    }
                }
            }

            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(bottom = 16.dp),
            ) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
                    placeholder = { Text("Message") },
                    modifier = Modifier.weight(1f),
                )
                Button(
                    onClick = {
                        val text = draft.trim()
                        if (text.isNotEmpty()) {
                            sender.sendText(contact, text)
                            draft = ""
                            reload()
                        }
                    },
                    modifier = Modifier.padding(start = 8.dp),
                ) {
                    Text("Send")
                }
            }
        }
    }
}

/**
 * One chat bubble: kind=1 payload decoded as UTF-8, right-aligned for own
 * messages, left-aligned for the contact's. [tick] is null for the
 * contact's messages (they never get one) and non-null for our own,
 * rendered as small right-aligned text next to the bubble text (DESIGN.md
 * §7.2): "✓" for [TickStatus.SENT], "✓✓" in the bubble's subdued content
 * color for [TickStatus.DELIVERED], "✓✓" tinted with
 * `MaterialTheme.colorScheme.primary` for [TickStatus.READ].
 */
@Composable
private fun MessageBubble(message: StoredMessage, isOwn: Boolean, tick: TickStatus?) {
    val text = remember(message) { message.payload.toString(Charsets.UTF_8) }
    val bubbleColor = if (isOwn) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceVariant
    val contentColor = if (isOwn) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurfaceVariant

    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
    ) {
        Surface(
            color = bubbleColor,
            contentColor = contentColor,
            shape = RoundedCornerShape(12.dp),
            modifier = Modifier.widthIn(max = 280.dp),
        ) {
            Row(
                verticalAlignment = Alignment.Bottom,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            ) {
                Text(text)
                if (tick != null) {
                    val (glyph, tint) = when (tick) {
                        TickStatus.SENT -> "✓" to contentColor
                        TickStatus.DELIVERED -> "✓✓" to contentColor.copy(alpha = 0.7f)
                        TickStatus.READ -> "✓✓" to MaterialTheme.colorScheme.primary
                    }
                    Text(
                        glyph,
                        style = MaterialTheme.typography.labelSmall,
                        color = tint,
                        modifier = Modifier.padding(start = 4.dp),
                    )
                }
            }
        }
    }
}
