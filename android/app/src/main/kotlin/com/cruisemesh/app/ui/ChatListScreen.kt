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
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.formatUserId

data class ChatSummary(
    val contact: Contact,
    val lastMessage: StoredMessage?,
    val unreadCount: Int,
    val ownDeliveredThrough: ULong,
    val ownReadThrough: ULong
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatListScreen(
    ownUserId: ByteArray,
    ownDisplayName: String,
    onChatClick: (Contact) -> Unit,
    onDeleteContact: (Contact) -> Unit,
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
                    items(summaries, key = { it.contact.userId.contentHashCode() }) { summary ->
                        var showDeleteDialog by remember { mutableStateOf(false) }

                        if (showDeleteDialog) {
                            AlertDialog(
                                onDismissRequest = { showDeleteDialog = false },
                                title = { Text("Delete Contact") },
                                text = { Text("Delete contact and all message history?") },
                                confirmButton = {
                                    TextButton(onClick = {
                                        showDeleteDialog = false
                                        onDeleteContact(summary.contact)
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
                            onClick = { onChatClick(summary.contact) },
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
    val displayId = remember(summary.contact.userId) { formatUserId(summary.contact.userId) }
    val displayName = remember(summary.contact.name, displayId) {
        ChatListLogic.displayNameOrId(summary.contact.name, displayId)
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
            userId = summary.contact.userId,
            name = summary.contact.name,
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
                    text = displayName,
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
                    val content = String(summary.lastMessage.payload, Charsets.UTF_8)
                    
                    if (isOwn) {
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
            onDeleteContact = {},
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
    val julesId = byteArrayOf(0x03, 0x04)
    CruiseMeshTheme {
        ChatListScreen(
            ownUserId = ownUserId,
            ownDisplayName = "Captain",
            onChatClick = {},
            onDeleteContact = {},
            onNewChatClick = {},
            onProfileClick = {},
            onMeshStatusClick = {},
            meshStatusText = "Meshing · 2 nearby",
            summaries = listOf(
                ChatSummary(
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
                    contact = Contact(
                        userId = julesId,
                        name = "Jules",
                        signPk = ByteArray(32),
                        agreePk = ByteArray(32),
                        relayUrl = null,
                        relayToken = null,
                    ),
                    lastMessage = StoredMessage(
                        senderUserId = ownUserId,
                        chatId = julesId,
                        lamport = 4uL,
                        timestamp = 1_783_610_400_000L,
                        kind = 1u.toUByte(),
                        payload = "We grabbed coffee already".toByteArray(),
                    ),
                    unreadCount = 0,
                    ownDeliveredThrough = 4uL,
                    ownReadThrough = 4uL,
                ),
            ),
        )
    }
}
