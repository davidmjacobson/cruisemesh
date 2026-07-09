package com.cruisemesh.app.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.cruisemesh.app.chat.TickStatus
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
                        Box(
                            modifier = Modifier
                                .size(32.dp)
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.primaryContainer),
                            contentAlignment = Alignment.Center
                        ) {
                            Text(
                                text = "ME",
                                color = MaterialTheme.colorScheme.onPrimaryContainer,
                                style = MaterialTheme.typography.labelMedium
                            )
                        }
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
            Surface(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onMeshStatusClick)
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                shape = CircleShape,
                color = MaterialTheme.colorScheme.secondaryContainer
            ) {
                Text(
                    text = meshStatusText,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                    textAlign = TextAlign.Center
                )
            }

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
    val (avatarColor, initials) = remember(summary.contact) {
        ChatListLogic.avatarHueAndInitials(summary.contact.userId, summary.contact.name, displayId)
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
        Box(
            modifier = Modifier
                .size(48.dp)
                .clip(CircleShape)
                .background(avatarColor),
            contentAlignment = Alignment.Center
        ) {
            Text(
                text = initials,
                color = Color.White,
                style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.Bold)
            )
        }
        
        Spacer(modifier = Modifier.width(16.dp))
        
        Column(modifier = Modifier.weight(1f)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Text(
                    text = if (summary.contact.name.isNotBlank() && summary.contact.name != "Unknown") summary.contact.name else displayId,
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
                        val glyph = when(tick) {
                            TickStatus.SENT -> "✓"
                            TickStatus.DELIVERED -> "✓✓"
                            TickStatus.READ -> "✓✓"
                        }
                        val tint = if (tick == TickStatus.READ) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant
                        Text(
                            text = glyph,
                            color = tint,
                            style = MaterialTheme.typography.labelSmall,
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
fun ChatListScreenPreview() {
    MaterialTheme {
        ChatListScreen(
            ownUserId = byteArrayOf(),
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
