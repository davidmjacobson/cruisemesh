package com.cruisemesh.app.chat

import android.content.res.Configuration
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
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
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.ui.AvatarBadge
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ContactDetailsSheet
import com.cruisemesh.app.ui.ConversationMessageMeta
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.SignalTick
import com.cruisemesh.app.ui.bubbleGroupingFor
import com.cruisemesh.app.ui.formatConversationTimestamp
import com.cruisemesh.app.ui.tickLegendText
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.formatUserId

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
    onDeleteContact: () -> Unit,
) {
    var messages by remember(contact.userId) { mutableStateOf(store.messagesForChat(contact.userId)) }
    var deliveredThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED))
    }
    var readThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ))
    }
    var draft by remember { mutableStateOf("") }

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

    ConversationScreen(
        contact = contact,
        ownUserId = ownUserId,
        messages = messages,
        deliveredThrough = deliveredThrough,
        readThrough = readThrough,
        draft = draft,
        onDraftChange = { draft = it },
        onSend = {
            val text = draft.trim()
            if (text.isNotEmpty()) {
                sender.sendText(contact, text)
                draft = ""
                reload()
            }
        },
        onBack = onBack,
        onDeleteContact = onDeleteContact,
    )
}

@Composable
private fun ConversationScreen(
    contact: Contact,
    ownUserId: ByteArray,
    messages: List<StoredMessage>,
    deliveredThrough: ULong,
    readThrough: ULong,
    draft: String,
    onDraftChange: (String) -> Unit,
    onSend: () -> Unit,
    onBack: () -> Unit,
    onDeleteContact: () -> Unit,
) {
    val listState = rememberLazyListState()
    val displayId = remember(contact.userId) { formatUserId(contact.userId) }
    val displayName = remember(contact.name, displayId) {
        ChatListLogic.displayNameOrId(contact.name, displayId)
    }
    val (contactColor, _) = remember(contact, displayId) {
        ChatListLogic.avatarHueAndInitials(contact.userId, contact.name, displayId)
    }
    val textMessages = remember(messages) { messages.filter { it.kind == KIND_TEXT } }
    val gaps = remember(textMessages) {
        val result = mutableSetOf<Int>()
        val lastLamport = mutableMapOf<String, ULong>()
        textMessages.forEachIndexed { index, msg ->
            val senderHex = formatUserId(msg.senderUserId)
            val previous = lastLamport[senderHex]
            if (previous != null && msg.lamport > previous + 1uL) {
                result.add(index)
            }
            lastLamport[senderHex] = maxOf(previous ?: 0uL, msg.lamport)
        }
        result
    }
    val grouping = remember(textMessages) {
        val meta = textMessages.map { ConversationMessageMeta(formatUserId(it.senderUserId), it.timestamp) }
        meta.indices.map { bubbleGroupingFor(meta, it) }
    }
    var showContactDetails by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }

    LaunchedEffect(textMessages.size) {
        if (textMessages.isNotEmpty()) {
            listState.animateScrollToItem(textMessages.size - 1)
        }
    }

    Scaffold(
        topBar = {
            ConversationTopBar(
                contact = contact,
                displayId = displayId,
                displayName = displayName,
                onBack = onBack,
                onOpenDetails = { showContactDetails = true },
            )
        }
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(horizontal = 16.dp)
        ) {
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f)
                    .padding(vertical = 8.dp),
            ) {
                itemsIndexed(
                    textMessages,
                    key = { _, message -> "${message.senderUserId.contentHashCode()}:${message.lamport}" },
                ) { index, message ->
                    val isOwn = message.senderUserId.contentEquals(ownUserId)

                    if (isNewDay(textMessages, index)) {
                        DaySeparator(message.timestamp)
                    }

                    if (gaps.contains(index)) {
                        GapIndicator()
                    }

                    MessageBubble(
                        message = message,
                        isOwn = isOwn,
                        tick = if (isOwn) tickStatusFor(message.lamport, deliveredThrough, readThrough) else null,
                        contactColor = if (isOwn) null else contactColor,
                        grouping = grouping[index],
                    )
                }
            }

            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 16.dp),
            ) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = onDraftChange,
                    placeholder = { Text("Message") },
                    modifier = Modifier.weight(1f),
                )
                Button(
                    onClick = onSend,
                    modifier = Modifier
                        .padding(start = 8.dp)
                        .height(56.dp),
                ) {
                    Text("Send")
                }
            }
        }
    }

    if (showContactDetails) {
        ContactDetailsSheet(
            contact = contact,
            onDeleteContact = {
                showContactDetails = false
                confirmDelete = true
            },
            onDismiss = { showContactDetails = false },
        )
    }

    if (confirmDelete) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text("Delete $displayName?") },
            text = { Text("This removes the contact and deletes your chat history with them.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmDelete = false
                        onDeleteContact()
                    },
                ) {
                    Text("Delete")
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = false }) {
                    Text("Cancel")
                }
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ConversationTopBar(
    contact: Contact,
    displayId: String,
    displayName: String,
    onBack: () -> Unit,
    onOpenDetails: () -> Unit,
) {
    TopAppBar(
        navigationIcon = {
            IconButton(onClick = onBack) {
                Icon(
                    imageVector = Icons.Default.ArrowBack,
                    contentDescription = "Back",
                )
            }
        },
        title = {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onOpenDetails)
                    .semantics {
                        role = Role.Button
                        contentDescription = "Contact details for $displayName"
                    },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AvatarBadge(
                    userId = contact.userId,
                    name = contact.name,
                    displayId = displayId,
                    size = 36.dp,
                )
                Spacer(modifier = Modifier.width(12.dp))
                Column {
                    Text(
                        text = displayName,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = "View contact details",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        },
    )
}

@Composable
private fun DaySeparator(timestampMs: Long) {
    val label = remember(timestampMs) {
        java.text.SimpleDateFormat("MMMM d, yyyy", java.util.Locale.US).format(java.util.Date(timestampMs))
    }
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun GapIndicator() {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = "some messages may still be in transit",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f),
        )
    }
}

@Composable
private fun MessageBubble(
    message: StoredMessage,
    isOwn: Boolean,
    tick: TickStatus?,
    contactColor: Color?,
    grouping: BubbleGrouping,
) {
    val text = remember(message) { message.payload.toString(Charsets.UTF_8) }
    val bubbleColor = if (isOwn) {
        MaterialTheme.colorScheme.primary
    } else {
        contactColor?.copy(alpha = 0.24f) ?: MaterialTheme.colorScheme.surfaceVariant
    }
    val contentColor = if (isOwn) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface
    val tickBaseColor = if (bubbleColor.luminance() > 0.5f) Color.Black else Color.White
    var showLegend by remember { mutableStateOf(false) }
    val topPadding = if (grouping.joinsPrevious) 2.dp else 10.dp
    val bottomPadding = if (grouping.joinsNext) 2.dp else 6.dp
    val shape = RoundedCornerShape(
        topStart = if (!isOwn && grouping.joinsPrevious) 6.dp else 20.dp,
        topEnd = if (isOwn && grouping.joinsPrevious) 6.dp else 20.dp,
        bottomStart = if (!isOwn && grouping.joinsNext) 6.dp else 20.dp,
        bottomEnd = if (isOwn && grouping.joinsNext) 6.dp else 20.dp,
    )

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = topPadding, bottom = bottomPadding),
        horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
    ) {
        Column(
            horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start,
            modifier = Modifier.widthIn(max = 300.dp),
        ) {
            Surface(
                color = bubbleColor,
                contentColor = contentColor,
                shape = shape,
                modifier = Modifier.clickable(enabled = tick != null) { showLegend = true },
            ) {
                Row(
                    verticalAlignment = Alignment.Bottom,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
                ) {
                    Text(text)
                    if (tick != null) {
                        val tint = when (tick) {
                            TickStatus.SENT -> tickBaseColor.copy(alpha = 0.88f)
                            TickStatus.DELIVERED -> tickBaseColor.copy(alpha = 0.74f)
                            TickStatus.READ -> tickBaseColor
                        }
                        SignalTick(
                            status = tick,
                            tint = tint,
                            bubbleColor = bubbleColor,
                            modifier = Modifier.padding(start = 8.dp, bottom = 2.dp),
                        )
                    }
                }
            }

            if (grouping.showTimestamp) {
                Text(
                    text = formatConversationTimestamp(message.timestamp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                )
            }
        }
    }

    if (showLegend && tick != null) {
        AlertDialog(
            onDismissRequest = { showLegend = false },
            title = { Text("Message Status") },
            text = {
                Text(tickLegendText(tick))
            },
            confirmButton = {
                TextButton(onClick = { showLegend = false }) { Text("OK") }
            }
        )
    }
}

private fun isNewDay(messages: List<StoredMessage>, index: Int): Boolean {
    val current = java.util.Calendar.getInstance().apply { timeInMillis = messages[index].timestamp }
    val previous = messages.getOrNull(index - 1)?.let {
        java.util.Calendar.getInstance().apply { timeInMillis = it.timestamp }
    }
    return previous == null ||
        current.get(java.util.Calendar.YEAR) != previous.get(java.util.Calendar.YEAR) ||
        current.get(java.util.Calendar.DAY_OF_YEAR) != previous.get(java.util.Calendar.DAY_OF_YEAR)
}

@Preview(showBackground = true, name = "Conversation")
@Preview(
    showBackground = true,
    name = "Conversation Dark",
    uiMode = Configuration.UI_MODE_NIGHT_YES,
)
@Composable
private fun ConversationScreenPreview() {
    val ownUserId = byteArrayOf(0x44, 0x11)
    val mayaId = byteArrayOf(0x01, 0x02)
    CruiseMeshTheme {
        ConversationScreen(
            contact = Contact(
                userId = mayaId,
                name = "Maya",
                signPk = ByteArray(32),
                agreePk = ByteArray(32),
                relayUrl = null,
                relayToken = null,
            ),
            ownUserId = ownUserId,
            messages = listOf(
                StoredMessage(mayaId, mayaId, 1uL, 1_783_608_000_000L, 1u.toUByte(), "Boarding now".toByteArray()),
                StoredMessage(mayaId, mayaId, 2uL, 1_783_608_090_000L, 1u.toUByte(), "Deck 9 looks quiet".toByteArray()),
                StoredMessage(ownUserId, mayaId, 3uL, 1_783_608_340_000L, 1u.toUByte(), "On my way".toByteArray()),
                StoredMessage(ownUserId, mayaId, 4uL, 1_783_608_420_000L, 1u.toUByte(), "Save me a seat".toByteArray()),
            ),
            deliveredThrough = 4uL,
            readThrough = 3uL,
            draft = "",
            onDraftChange = {},
            onSend = {},
            onBack = {},
            onDeleteContact = {},
        )
    }
}
