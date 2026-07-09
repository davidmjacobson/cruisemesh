package com.cruisemesh.app.ui

import android.content.res.Configuration
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.Button
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.cruisemesh.app.chat.tickStatusFor
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.formatUserId

/**
 * One row on the home conversation list — either a 1:1 contact chat or a
 * group (DESIGN.md §14.2 / §14.6).
 */
data class ChatSummary(
    val chatId: ByteArray,
    val title: String,
    val isGroup: Boolean,
    val contact: Contact? = null,
    val group: Group? = null,
    val lastMessage: StoredMessage?,
    val unreadCount: Int,
    val ownDeliveredThrough: ULong,
    val ownReadThrough: ULong,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ChatSummary) return false
        return chatId.contentEquals(other.chatId) &&
            title == other.title &&
            isGroup == other.isGroup &&
            lastMessage == other.lastMessage &&
            unreadCount == other.unreadCount &&
            ownDeliveredThrough == other.ownDeliveredThrough &&
            ownReadThrough == other.ownReadThrough
    }

    override fun hashCode(): Int = chatId.contentHashCode()
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatListScreen(
    ownUserId: ByteArray,
    ownDisplayName: String,
    onChatClick: (ChatSummary) -> Unit,
    onDeleteSummary: (ChatSummary) -> Unit,
    onNewChatClick: () -> Unit,
    onProfileClick: () -> Unit,
    onMeshStatusClick: () -> Unit,
    meshStatusText: String,
    summaries: List<ChatSummary>
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("CruiseMesh") },
                navigationIcon = {
                    IconButton(onClick = onProfileClick) {
                        AvatarBadge(
                            userId = ownUserId,
                            name = ownDisplayName,
                            displayId = formatUserId(ownUserId),
                            size = 32.dp,
                        )
                    }
                },
                actions = {
                    var showMenu by remember { mutableStateOf(false) }
                    IconButton(onClick = { showMenu = true }) {
                        Icon(Icons.Default.MoreVert, contentDescription = "More")
                    }
                    DropdownMenu(
                        expanded = showMenu,
                        onDismissRequest = { showMenu = false }
                    ) {
                        DropdownMenuItem(
                            text = { Text("Friends") },
                            onClick = { 
                                showMenu = false
                                onNewChatClick() 
                            }
                        )
                    }
                }
            )
        },
        floatingActionButton = {
            FloatingActionButton(onClick = onNewChatClick) {
                Icon(Icons.Default.Add, contentDescription = "New Chat")
            }
        }
    ) { innerPadding ->
        Column(modifier = Modifier.padding(innerPadding).fillMaxSize()) {
            MeshStatusPill(
                text = meshStatusText,
                onClick = onMeshStatusClick,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )

            if (summaries.isEmpty()) {
                Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(
                            text = "No conversations yet",
                            style = MaterialTheme.typography.titleMedium
                        )
                        Button(
                            onClick = onNewChatClick,
                            modifier = Modifier.padding(top = 16.dp)
                        ) {
                            Text("Add a friend")
                        }
                    }
                }
            } else {
                LazyColumn(modifier = Modifier.fillMaxSize()) {
                    items(summaries, key = { it.chatId.contentHashCode() }) { summary ->
                        var showDeleteDialog by remember { mutableStateOf(false) }

                        if (showDeleteDialog) {
                            AlertDialog(
                                onDismissRequest = { showDeleteDialog = false },
                                title = {
                                    Text(if (summary.isGroup) "Delete group" else "Delete Contact")
                                },
                                text = {
                                    Text(
                                        if (summary.isGroup) {
                                            "Delete this group and its message history from this device?"
                                        } else {
                                            "Delete contact and all message history?"
                                        },
                                    )
                                },
                                confirmButton = {
                                    TextButton(onClick = {
                                        showDeleteDialog = false
                                        onDeleteSummary(summary)
                                    }) { Text("Delete") }
                                },
                                dismissButton = {
                                    TextButton(onClick = { showDeleteDialog = false }) { Text("Cancel") }
                                }
                            )
                        }

                        ChatRow(
                            summary = summary,
                            ownUserId = ownUserId,
                            onClick = { onChatClick(summary) },
                            onLongClick = { showDeleteDialog = true }
                        )
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun ChatRow(
    summary: ChatSummary,
    ownUserId: ByteArray,
    onClick: () -> Unit,
    onLongClick: () -> Unit
) {
    val displayId = remember(summary.chatId, summary.isGroup) {
        if (summary.isGroup) summary.title else formatUserId(summary.chatId)
    }
    val displayName = remember(summary.title, displayId) {
        ChatListLogic.displayNameOrId(summary.title, displayId)
    }
    val isUnread = summary.unreadCount > 0

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(
                onClick = onClick,
                onLongClick = onLongClick
            )
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        AvatarBadge(
            userId = summary.chatId,
            name = summary.title,
            displayId = displayId,
        )

        Spacer(modifier = Modifier.width(16.dp))

        Column(modifier = Modifier.weight(1f)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Text(
                    text = if (summary.isGroup) "👥 $displayName" else displayName,
                    style = MaterialTheme.typography.bodyLarge.copy(
                        fontWeight = if (isUnread) FontWeight.Bold else FontWeight.Normal
                    ),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f)
                )
                
                if (summary.lastMessage != null) {
                    Text(
                        text = ChatListLogic.formatRelativeTime(summary.lastMessage.timestamp),
                        style = MaterialTheme.typography.labelSmall,
                        color = if (isUnread) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(start = 8.dp)
                    )
                }
            }
            
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically
            ) {
                if (summary.lastMessage != null) {
                    val isOwn = summary.lastMessage.senderUserId.contentEquals(ownUserId)
                    val prefix = if (isOwn) "You: " else ""
                    val content = ChatListLogic.previewText(
                        summary.lastMessage,
                        groupName = if (summary.isGroup) summary.title else null,
                    )
                    
                    if (isOwn && !summary.isGroup) {
                        val tick = tickStatusFor(
                            summary.lastMessage.lamport,
                            summary.ownDeliveredThrough,
                            summary.ownReadThrough
                        )
                        val tint = if (summary.ownReadThrough >= summary.lastMessage.lamport) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        }
                        SignalTick(
                            status = tick,
                            tint = tint,
                            bubbleColor = MaterialTheme.colorScheme.surface,
                            modifier = Modifier.padding(end = 4.dp)
                        )
                    }

                    Text(
                        text = prefix + content,
                        style = MaterialTheme.typography.bodyMedium.copy(
                            fontWeight = if (isUnread) FontWeight.Bold else FontWeight.Normal
                        ),
                        color = if (isUnread) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f)
                    )
                }
                
                if (summary.unreadCount > 0) {
                    Box(
                        modifier = Modifier
                            .padding(start = 8.dp)
                            .size(20.dp)
                            .clip(CircleShape)
                            .background(MaterialTheme.colorScheme.primary),
                        contentAlignment = Alignment.Center
                    ) {
                        Text(
                            text = summary.unreadCount.toString(),
                            color = MaterialTheme.colorScheme.onPrimary,
                            style = MaterialTheme.typography.labelSmall.copy(fontSize = 10.sp)
                        )
                    }
                }
            }
        }
    }
}

@Preview(showBackground = true)
@Composable
private fun ChatListScreenEmptyPreview() {
    CruiseMeshTheme {
        ChatListScreen(
            ownUserId = byteArrayOf(0x44, 0x11),
            ownDisplayName = "Captain",
            onChatClick = {},
            onDeleteSummary = {},
            onNewChatClick = {},
            onProfileClick = {},
            onMeshStatusClick = {},
            meshStatusText = "Mesh off",
            summaries = emptyList()
        )
    }
}

@Preview(showBackground = true, name = "Chat List")
@Preview(
    showBackground = true,
    name = "Chat List Dark",
    uiMode = Configuration.UI_MODE_NIGHT_YES,
)
@Composable
private fun ChatListScreenPreview() {
    val ownUserId = byteArrayOf(0x44, 0x11)
    val mayaId = byteArrayOf(0x01, 0x02)
    val groupId = ByteArray(16) { 0x11 }
    CruiseMeshTheme {
        ChatListScreen(
            ownUserId = ownUserId,
            ownDisplayName = "Captain",
            onChatClick = {},
            onDeleteSummary = {},
            onNewChatClick = {},
            onProfileClick = {},
            onMeshStatusClick = {},
            meshStatusText = "Meshing · 2 nearby",
            summaries = listOf(
                ChatSummary(
                    chatId = mayaId,
                    title = "Maya",
                    isGroup = false,
                    contact = Contact(
                        userId = mayaId,
                        name = "Maya",
                        signPk = ByteArray(32),
                        agreePk = ByteArray(32),
                        relayUrl = null,
                        relayToken = null,
                    ),
                    lastMessage = StoredMessage(
                        senderUserId = mayaId,
                        chatId = mayaId,
                        lamport = 8uL,
                        timestamp = 1_783_614_000_000L,
                        kind = 1u.toUByte(),
                        payload = "Meet us by the aft elevators".toByteArray(),
                    ),
                    unreadCount = 2,
                    ownDeliveredThrough = 0uL,
                    ownReadThrough = 0uL,
                ),
                ChatSummary(
                    chatId = groupId,
                    title = "Bridge Crew",
                    isGroup = true,
                    group = Group(
                        id = groupId,
                        name = "Bridge Crew",
                        memberUserIds = listOf(ownUserId, mayaId),
                        key = ByteArray(32) { 0x22 },
                    ),
                    lastMessage = StoredMessage(
                        senderUserId = ownUserId,
                        chatId = groupId,
                        lamport = 2uL,
                        timestamp = 1_783_615_000_000L,
                        kind = 1u.toUByte(),
                        payload = "Dinner at 7?".toByteArray(),
                    ),
                    unreadCount = 0,
                    ownDeliveredThrough = 0uL,
                    ownReadThrough = 0uL,
                ),
            )
        )
    }
}
