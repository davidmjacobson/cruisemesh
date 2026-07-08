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
import kotlinx.coroutines.delay
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/** Interim refresh cadence until [com.cruisemesh.app.mesh.MeshService] can push updates directly into this screen. */
private const val POLL_INTERVAL_MS = 1_000L

/**
 * A single 1:1 chat thread (DESIGN.md §7.1: for a 1:1 chat, `chat_id` is
 * simply the peer's UserID). Renders `kind=1` (text) messages oldest-first,
 * auto-scrolled to the newest, with the local user's bubbles right-aligned
 * and the contact's left-aligned (compared via [ByteArray.contentEquals]
 * against `ownUserId`, since [StoredMessage.senderUserId] is raw bytes).
 *
 * Sending goes through [sender] only -- see [MeshSender] for why the UI
 * never talks to a concrete transport directly. After a send, and on a
 * polling timer (interim until MeshService can push live updates -- the
 * next milestone), the thread is reloaded from [store].
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
    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()

    // Interim polling loop: reload the thread on a timer so incoming
    // messages (once MeshService writes them into the store) show up
    // without a manual refresh. Replace with a push/observer once
    // MeshService can notify the UI directly.
    LaunchedEffect(contact.userId) {
        while (true) {
            delay(POLL_INTERVAL_MS)
            messages = store.messagesForChat(contact.userId)
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
                        MessageBubble(message, isOwn = message.senderUserId.contentEquals(ownUserId))
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
                            messages = store.messagesForChat(contact.userId)
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

/** One chat bubble: kind=1 payload decoded as UTF-8, right-aligned for own messages, left-aligned for the contact's. */
@Composable
private fun MessageBubble(message: StoredMessage, isOwn: Boolean) {
    val text = remember(message) { message.payload.toString(Charsets.UTF_8) }

    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
    ) {
        Surface(
            color = if (isOwn) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceVariant,
            contentColor = if (isOwn) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurfaceVariant,
            shape = RoundedCornerShape(12.dp),
            modifier = Modifier.widthIn(max = 280.dp),
        ) {
            Text(text, modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp))
        }
    }
}
