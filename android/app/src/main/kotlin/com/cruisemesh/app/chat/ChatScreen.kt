package com.cruisemesh.app.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.clickable
import androidx.compose.material3.AlertDialog
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.text.font.FontWeight
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

    val (contactColor, _) = remember(contact) {
        com.cruisemesh.app.ui.ChatListLogic.avatarHueAndInitials(contact.userId, contact.name, uniffi.cruisemesh_core.formatUserId(contact.userId))
    }

    val gaps = remember(messages) {
        val result = mutableSetOf<Int>()
        val lastLamport = mutableMapOf<String, ULong>()
        messages.forEachIndexed { i, msg ->
            if (msg.kind == KIND_TEXT) {
                val senderHex = uniffi.cruisemesh_core.formatUserId(msg.senderUserId)
                val prev = lastLamport[senderHex]
                if (prev != null && msg.lamport > prev + 1uL) {
                    result.add(i)
                }
                lastLamport[senderHex] = maxOf(prev ?: 0uL, msg.lamport)
            }
        }
        result
    }

    fun reload() {
        messages = store.messagesForChat(contact.userId)
        deliveredThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED)
        readThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ)
    }

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
                itemsIndexed(messages, key = { _, it -> "${it.senderUserId.contentHashCode()}:${it.lamport}" }) { i, message ->
                    if (message.kind == KIND_TEXT) {
                        val isOwn = message.senderUserId.contentEquals(ownUserId)

                        val cal = remember(message.timestamp) { java.util.Calendar.getInstance().apply { timeInMillis = message.timestamp } }
                        val prevCal = if (i > 0) java.util.Calendar.getInstance().apply { timeInMillis = messages[i-1].timestamp } else null
                        val isNewDay = prevCal == null || 
                            cal.get(java.util.Calendar.DAY_OF_YEAR) != prevCal.get(java.util.Calendar.DAY_OF_YEAR) || 
                            cal.get(java.util.Calendar.YEAR) != prevCal.get(java.util.Calendar.YEAR)

                        if (isNewDay) {
                            val dateString = java.text.SimpleDateFormat("MMMM d, yyyy", java.util.Locale.US).format(cal.time)
                            Text(
                                dateString,
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp).wrapContentWidth(Alignment.CenterHorizontally)
                            )
                        }

                        if (gaps.contains(i)) {
                            Text(
                                "some messages may still be in transit",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f),
                                modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp).wrapContentWidth(Alignment.CenterHorizontally)
                            )
                        }

                        MessageBubble(
                            message = message,
                            isOwn = isOwn,
                            tick = if (isOwn) tickStatusFor(message.lamport, deliveredThrough, readThrough) else null,
                            contactColor = if (isOwn) null else contactColor
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

@Composable
private fun MessageBubble(message: StoredMessage, isOwn: Boolean, tick: TickStatus?, contactColor: Color?) {
    val text = remember(message) { message.payload.toString(Charsets.UTF_8) }
    val bubbleColor = if (isOwn) MaterialTheme.colorScheme.primary else (contactColor?.copy(alpha = 0.2f) ?: MaterialTheme.colorScheme.surfaceVariant)
    val contentColor = if (isOwn) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface
    val tickBaseColor = if (bubbleColor.luminance() > 0.5f) Color.Black else Color.White
    val tickChipColor =
        if (tickBaseColor == Color.White) Color.Black.copy(alpha = 0.28f) else Color.White.copy(alpha = 0.34f)
    var showLegend by remember { mutableStateOf(false) }

    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
    ) {
        Surface(
            color = bubbleColor,
            contentColor = contentColor,
            shape = RoundedCornerShape(12.dp),
            modifier = Modifier.widthIn(max = 280.dp).clickable { if (isOwn) showLegend = true },
        ) {
            Row(
                verticalAlignment = Alignment.Bottom,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            ) {
                Text(text)
                if (tick != null) {
                    val (glyph, tint) = when (tick) {
                        TickStatus.SENT -> "✓" to tickBaseColor.copy(alpha = 0.85f)
                        TickStatus.DELIVERED -> "✓✓" to tickBaseColor.copy(alpha = 0.7f)
                        TickStatus.READ -> "✓✓" to tickBaseColor
                    }
                    Surface(
                        color = tickChipColor,
                        shape = CircleShape,
                        modifier = Modifier.padding(start = 6.dp),
                    ) {
                        Text(
                            glyph,
                            style = MaterialTheme.typography.labelSmall.copy(fontWeight = FontWeight.SemiBold),
                            color = tint,
                            modifier = Modifier.padding(horizontal = 5.dp, vertical = 1.dp),
                        )
                    }
                }
            }
        }
    }

    if (showLegend && tick != null) {
        AlertDialog(
            onDismissRequest = { showLegend = false },
            title = { Text("Message Status") },
            text = { 
                val statusText = when(tick) {
                    TickStatus.SENT -> "✓ Sent: queued for delivery."
                    TickStatus.DELIVERED -> "✓✓ Delivered: received by the contact's device."
                    TickStatus.READ -> "✓✓ Read: viewed by the contact."
                }
                Text(statusText)
            },
            confirmButton = {
                TextButton(onClick = { showLegend = false }) { Text("OK") }
            }
        )
    }
}
