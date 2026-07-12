package com.cruisemesh.app.chat

import android.widget.Toast
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
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.media.KIND_GROUP_INVITE
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.ui.AvatarBadge
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ConversationMessageMeta
import com.cruisemesh.app.ui.bubbleGroupingFor
import com.cruisemesh.app.ui.formatConversationTimestamp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.formatUserId

/**
 * Group chat thread (DESIGN.md §6.5). Local `chat_id` is the group id.
 * Group wire receipts are deferred — no ✓/✓✓ ticks yet.
 */
@Composable
fun GroupChatScreen(
    group: Group,
    ownUserId: ByteArray,
    contactsByUserId: Map<String, Contact>,
    sender: GroupSender,
    store: MessageStore,
    onBack: () -> Unit,
    onDeleteGroup: () -> Unit,
    reachableMemberCount: Int? = null,
) {
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    val density = LocalDensity.current
    val keyboardFreeze = rememberOverlayKeyboardFreeze()
    var messages by remember(group.id) { mutableStateOf(store.messagesForChat(group.id)) }
    var draft by remember { mutableStateOf("") }
    var showDetails by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }
    var focused by remember(group.id) { mutableStateOf<FocusedMessage?>(null) }
    var infoMessage by remember(group.id) { mutableStateOf<StoredMessage?>(null) }

    fun reload() {
        messages = store.messagesForChat(group.id)
    }

    fun senderName(userId: ByteArray): String {
        if (userId.contentEquals(ownUserId)) return "You"
        val contact = contactsByUserId[UserIdHex.encode(userId)]
        return contact?.name?.takeIf { it.isNotBlank() }
            ?: formatUserId(userId)
    }

    LaunchedEffect(group.id) {
        ChatEvents.changes.collect { changedChatId ->
            if (changedChatId.contentEquals(group.id)) {
                reload()
            }
        }
    }

    val listState = rememberLazyListState()
    val visibleMessages = remember(messages) { messages.filter { isVisibleChatKind(it.kind) } }
    val reactions = remember(messages, ownUserId) { reactionSummariesByTarget(messages, ownUserId) }
    val grouping = remember(visibleMessages) {
        val meta = visibleMessages.map { ConversationMessageMeta(formatUserId(it.senderUserId), it.timestamp) }
        meta.indices.map { bubbleGroupingFor(meta, it) }
    }
    // Newest-first for reverseLayout LazyColumn: index 0 sits at the bottom
    // edge (just above the composer / keyboard), empty space stays above.
    val displayMessages = remember(visibleMessages) { visibleMessages.asReversed() }

    fun toggleReaction(target: MessageTarget, emoji: String) {
        val existingOwn = reactions[target.stableKey].orEmpty().firstOrNull { it.emoji == emoji && it.reactedByOwnUser }
        sender.sendReaction(group, target, if (existingOwn != null) "" else emoji)
        reload()
    }

    // The overlay takes over the full screen, so drop the keyboard while it's
    // open and bring it back once closed. OverlayKeyboardFreeze keeps the
    // conversation pixel-frozen while the keyboard animates, Signal-style.
    fun openOverlay(target: MessageTarget, bounds: Rect) {
        keyboardFreeze.onOverlayOpened()
        focused = FocusedMessage(target, bounds)
    }

    fun closeOverlay() {
        focused = null
        keyboardFreeze.onOverlayClosed()
    }

    val overlayOpen = focused != null
    LaunchedEffect(overlayOpen) {
        if (!overlayOpen) {
            keyboardFreeze.releaseWhenKeyboardReturns()
        }
    }

    LaunchedEffect(visibleMessages.size) {
        if (visibleMessages.isNotEmpty()) {
            // reverseLayout start is the bottom; pin the newest message there.
            listState.scrollToItem(0)
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
    Scaffold(
        topBar = {
            GroupConversationTopBar(
                group = group,
                memberCount = group.memberUserIds.size,
                reachableMemberCount = reachableMemberCount,
                onBack = onBack,
                onOpenDetails = { showDetails = true },
            )
        },
    ) { innerPadding ->
        // Scaffold's contentWindowInsets (safeDrawing) already include IME, so
        // do not also call imePadding() here — that double-counts keyboard height.
        // keyboardFreeze mirrors this same live bottom inset back to itself so it
        // can pin the total constant while the overlay is open (see OverlayKeyboardFreeze).
        val bottomInsetPx = with(density) { innerPadding.calculateBottomPadding().toPx() }
        SideEffect { keyboardFreeze.trackLiveBottomInset(bottomInsetPx) }
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(bottom = with(density) { keyboardFreeze.extraBottomPx.toDp() })
                .padding(horizontal = 16.dp),
        ) {
            LazyColumn(
                state = listState,
                reverseLayout = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f)
                    .padding(vertical = 8.dp),
            ) {
                itemsIndexed(
                    displayMessages,
                    key = { _, message -> "${message.senderUserId.contentHashCode()}:${message.lamport}" },
                ) { revIndex, message ->
                    val index = visibleMessages.lastIndex - revIndex
                    val isOwn = message.senderUserId.contentEquals(ownUserId)
                    GroupMessageBubble(
                        message = message,
                        isOwn = isOwn,
                        senderLabel = if (!isOwn && !grouping[index].joinsPrevious) {
                            senderName(message.senderUserId)
                        } else {
                            null
                        },
                        groupName = group.name,
                        grouping = grouping[index],
                        reactions = reactions[MessageTarget(message.senderUserId, message.lamport, message.kind).stableKey].orEmpty(),
                        onReact = { emoji ->
                            toggleReaction(MessageTarget(message.senderUserId, message.lamport, message.kind), emoji)
                        },
                        onLongPress = { target, bounds -> openOverlay(target, bounds) },
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
                    onValueChange = { draft = it },
                    placeholder = { Text("Message") },
                    modifier = Modifier.weight(1f),
                )
                Button(
                    onClick = {
                        val text = draft.trim()
                        if (text.isNotEmpty()) {
                            sender.sendText(group, text)
                            draft = ""
                            reload()
                        }
                    },
                    modifier = Modifier
                        .padding(start = 8.dp)
                        .height(56.dp),
                ) {
                    Text("Send")
                }
            }
        }
    }

    if (showDetails) {
        AlertDialog(
            onDismissRequest = { showDetails = false },
            title = { Text(group.name) },
            text = {
                Column {
                    Text(
                        "${group.memberUserIds.size} members",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(bottom = 8.dp),
                    )
                    for (memberId in group.memberUserIds) {
                        Text(
                            "• ${senderName(memberId)}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        showDetails = false
                        confirmDelete = true
                    },
                ) { Text("Leave / delete") }
            },
            dismissButton = {
                TextButton(onClick = { showDetails = false }) { Text("Close") }
            },
        )
    }

    if (confirmDelete) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text("Delete ${group.name}?") },
            text = {
                Text("Removes this group and its message history from this device. Other members keep their copy.")
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmDelete = false
                        onDeleteGroup()
                    },
                ) { Text("Delete") }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = false }) { Text("Cancel") }
            },
        )
    }

    val currentFocused = focused
    if (currentFocused != null) {
        val focusedMessage = visibleMessages.firstOrNull {
            MessageTarget(it.senderUserId, it.lamport, it.kind).stableKey == currentFocused.target.stableKey
        }
        // focusedMessage is null only if the message vanished from under us
        // (e.g. deleted) while the overlay was open; just render nothing.
        if (focusedMessage != null) {
            val focusedIsOwn = focusedMessage.senderUserId.contentEquals(ownUserId)
            val focusedIndex = visibleMessages.indexOf(focusedMessage)
            val focusedGrouping = grouping.getOrNull(focusedIndex) ?: BubbleGrouping(joinsPrevious = false, joinsNext = false)
            val focusedShape = bubbleShapeFor(focusedIsOwn, focusedGrouping)
            val focusedReactions = reactions[currentFocused.target.stableKey].orEmpty()
            val focusedCopyText = remember(focusedMessage.payload) { String(focusedMessage.payload, Charsets.UTF_8) }
            val focusedOwnReaction = focusedReactions.firstOrNull { it.reactedByOwnUser }?.emoji
            val focusedSenderLabel = if (!focusedIsOwn) senderName(focusedMessage.senderUserId) else null

            MessageFocusOverlay(
                focused = currentFocused,
                isOwn = focusedIsOwn,
                canCopy = focusedCopyText.isNotBlank(),
                ownReactionEmoji = focusedOwnReaction,
                onDismiss = { closeOverlay() },
                onReact = { emoji ->
                    toggleReaction(currentFocused.target, emoji)
                    closeOverlay()
                },
                onCopy = {
                    if (focusedCopyText.isNotBlank()) {
                        clipboard.setText(AnnotatedString(focusedCopyText))
                        Toast.makeText(context, "Copied", Toast.LENGTH_SHORT).show()
                    }
                    closeOverlay()
                },
                onInfo = {
                    infoMessage = focusedMessage
                    closeOverlay()
                },
            ) {
                GroupMessageBubbleVisual(
                    message = focusedMessage,
                    isOwn = focusedIsOwn,
                    senderLabel = focusedSenderLabel,
                    shape = focusedShape,
                    showTimestamp = focusedGrouping.showTimestamp,
                    reactions = focusedReactions,
                    onReact = { emoji ->
                        toggleReaction(currentFocused.target, emoji)
                        closeOverlay()
                    },
                )
            }
        }
    }
    } // Box

    val currentInfoMessage = infoMessage
    if (currentInfoMessage != null) {
        AlertDialog(
            onDismissRequest = { infoMessage = null },
            title = { Text("Message Info") },
            text = { Text(messageInfoText(currentInfoMessage, currentInfoMessage.senderUserId.contentEquals(ownUserId), null)) },
            confirmButton = {
                TextButton(onClick = { infoMessage = null }) { Text("OK") }
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GroupConversationTopBar(
    group: Group,
    memberCount: Int,
    reachableMemberCount: Int?,
    onBack: () -> Unit,
    onOpenDetails: () -> Unit,
) {
    TopAppBar(
        navigationIcon = {
            IconButton(onClick = onBack) {
                Icon(Icons.Default.ArrowBack, contentDescription = "Back")
            }
        },
        title = {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                AvatarBadge(
                    userId = group.id,
                    name = group.name,
                    displayId = group.name,
                    size = 36.dp,
                )
                Spacer(modifier = Modifier.width(12.dp))
                Column(
                    modifier = Modifier
                        .weight(1f)
                        .padding(end = 8.dp),
                ) {
                    Text(
                        text = group.name,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    val subtitle = if (reachableMemberCount != null) {
                        "$reachableMemberCount of $memberCount reachable"
                    } else {
                        "$memberCount members · tap for details"
                    }
                    Text(
                        text = subtitle,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 0.dp),
                    )
                }
            }
        },
        actions = {
            TextButton(onClick = onOpenDetails) { Text("Info") }
        },
    )
}

@Composable
private fun GroupMessageBubble(
    message: StoredMessage,
    isOwn: Boolean,
    senderLabel: String?,
    groupName: String,
    grouping: BubbleGrouping,
    reactions: List<ReactionSummary> = emptyList(),
    onReact: (String) -> Unit = {},
    onLongPress: (MessageTarget, Rect) -> Unit = { _, _ -> },
) {
    if (message.kind == KIND_GROUP_INVITE) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 8.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = ChatListLogic.previewText(message, groupName),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }

    var boundsInRoot by remember { mutableStateOf(Rect.Zero) }
    val topPadding = if (grouping.joinsPrevious) 2.dp else 10.dp
    val bottomPadding = if (grouping.joinsNext) 2.dp else 6.dp
    val shape = bubbleShapeFor(isOwn, grouping)
    val target = remember(message.senderUserId, message.lamport, message.kind) {
        MessageTarget(message.senderUserId, message.lamport, message.kind)
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = topPadding, bottom = bottomPadding),
        horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
    ) {
        GroupMessageBubbleVisual(
            message = message,
            isOwn = isOwn,
            senderLabel = senderLabel,
            shape = shape,
            showTimestamp = grouping.showTimestamp,
            reactions = reactions,
            onReact = onReact,
            modifier = Modifier
                .onGloballyPositioned { coords -> boundsInRoot = coords.boundsInRoot() }
                .messageActions(
                    onLongClick = { onLongPress(target, boundsInRoot) },
                ),
        )
    }
}

/**
 * The group bubble's visual only -- sender label, Surface with text + inline
 * timestamp, and the reaction chips below, no click handling. Used both by
 * the list item ([GroupMessageBubble]) and by [MessageFocusOverlay]'s
 * undimmed floating copy.
 */
@Composable
fun GroupMessageBubbleVisual(
    message: StoredMessage,
    isOwn: Boolean,
    senderLabel: String?,
    shape: RoundedCornerShape,
    showTimestamp: Boolean,
    reactions: List<ReactionSummary>,
    onReact: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val bubbleColor = if (isOwn) {
        MaterialTheme.colorScheme.primary
    } else {
        MaterialTheme.colorScheme.surfaceVariant
    }
    val contentColor = if (isOwn) {
        MaterialTheme.colorScheme.onPrimary
    } else {
        MaterialTheme.colorScheme.onSurface
    }

    Column(
        horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start,
        modifier = modifier.widthIn(max = 280.dp),
    ) {
        if (senderLabel != null) {
            Text(
                text = senderLabel,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 12.dp, bottom = 2.dp),
            )
        }
        Surface(
            shape = shape,
            color = bubbleColor,
            contentColor = contentColor,
        ) {
            Column(modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp)) {
                Text(
                    text = String(message.payload, Charsets.UTF_8),
                    style = MaterialTheme.typography.bodyLarge,
                )
                if (showTimestamp) {
                    Text(
                        text = formatConversationTimestamp(message.timestamp),
                        style = MaterialTheme.typography.labelSmall,
                        color = contentColor.copy(alpha = 0.7f),
                        modifier = Modifier
                            .align(Alignment.End)
                            .padding(top = 4.dp),
                    )
                }
            }
        }
        if (reactions.isNotEmpty()) {
            ReactionRow(
                reactions = reactions,
                isOwn = isOwn,
                onReact = onReact,
            )
        }
    }
}
