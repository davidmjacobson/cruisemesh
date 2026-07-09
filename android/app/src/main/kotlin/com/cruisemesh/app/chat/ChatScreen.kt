package com.cruisemesh.app.chat

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.BitmapFactory
import android.media.MediaPlayer
import android.net.Uri
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.MediaCompressor
import com.cruisemesh.app.media.VoiceRecorder
import com.cruisemesh.app.media.isVisibleChatKind
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
import java.io.File
import kotlinx.coroutines.delay

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/** `receipt_type` values (DESIGN.md §7.2), for reading own-message tick watermarks out of the store. */
private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u

private const val MAX_VOICE_MS = 60_000

/**
 * A single 1:1 chat thread (DESIGN.md §7.1: for a 1:1 chat, `chat_id` is
 * simply the peer's UserID). Renders visible chat kinds (text + attachment
 * manifests) oldest-first, auto-scrolled to the newest, with the local user's
 * bubbles right-aligned and the contact's left-aligned (compared via
 * [ByteArray.contentEquals] against `ownUserId`, since
 * [StoredMessage.senderUserId] is raw bytes).
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
    val context = LocalContext.current
    var messages by remember(contact.userId) { mutableStateOf(store.messagesForChat(contact.userId)) }
    var deliveredThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED))
    }
    var readThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ))
    }
    var draft by remember { mutableStateOf("") }
    var attachMenuOpen by remember { mutableStateOf(false) }
    var showVoiceDialog by remember { mutableStateOf(false) }
    var pendingCameraUri by remember { mutableStateOf<Uri?>(null) }
    val voiceRecorder = remember { VoiceRecorder(context) }

    fun reload() {
        messages = store.messagesForChat(contact.userId)
        deliveredThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED)
        readThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ)
    }

    fun sendImageBytes(jpeg: ByteArray?) {
        if (jpeg == null) {
            Toast.makeText(context, "Could not prepare photo (too large or unreadable)", Toast.LENGTH_SHORT).show()
            return
        }
        sender.sendAttachment(
            contact,
            AttachmentPayload(
                mediaType = AttachmentPayload.MediaType.IMAGE,
                mimeType = "image/jpeg",
                durationMs = 0,
                blob = jpeg,
            ),
        )
        reload()
    }

    fun sendVoiceFile(file: File, durationMs: Int) {
        val bytes = try {
            file.readBytes()
        } catch (_: Exception) {
            null
        }
        file.delete()
        if (bytes == null || bytes.isEmpty()) {
            Toast.makeText(context, "Could not save voice memo", Toast.LENGTH_SHORT).show()
            return
        }
        if (bytes.size > AttachmentPayload.MAX_BLOB_BYTES) {
            Toast.makeText(context, "Voice memo is too large to send over the mesh", Toast.LENGTH_SHORT).show()
            return
        }
        sender.sendAttachment(
            contact,
            AttachmentPayload(
                mediaType = AttachmentPayload.MediaType.AUDIO,
                mimeType = "audio/mp4",
                durationMs = durationMs.coerceAtMost(MAX_VOICE_MS),
                blob = bytes,
            ),
        )
        reload()
    }

    val galleryLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            sendImageBytes(MediaCompressor.compressImageUri(context, uri))
        }
    }

    val cameraLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { success ->
        val uri = pendingCameraUri
        pendingCameraUri = null
        if (success && uri != null) {
            sendImageBytes(MediaCompressor.compressImageUri(context, uri))
        }
    }

    val cameraPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            launchCamera(context) { uri ->
                pendingCameraUri = uri
                cameraLauncher.launch(uri)
            }
        } else {
            Toast.makeText(context, "Camera permission is required to take photos", Toast.LENGTH_SHORT).show()
        }
    }

    val micPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            showVoiceDialog = true
        } else {
            Toast.makeText(context, "Microphone permission is required for voice memos", Toast.LENGTH_SHORT).show()
        }
    }

    DisposableEffect(Unit) {
        onDispose { voiceRecorder.cancel() }
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
        attachMenuOpen = attachMenuOpen,
        onAttachMenuChange = { attachMenuOpen = it },
        onPickGallery = {
            attachMenuOpen = false
            galleryLauncher.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
        },
        onPickCamera = {
            attachMenuOpen = false
            val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED
            if (granted) {
                launchCamera(context) { uri ->
                    pendingCameraUri = uri
                    cameraLauncher.launch(uri)
                }
            } else {
                cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
            }
        },
        onPickVoice = {
            attachMenuOpen = false
            val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED
            if (granted) {
                showVoiceDialog = true
            } else {
                micPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
            }
        },
        onBack = onBack,
        onDeleteContact = onDeleteContact,
    )

    if (showVoiceDialog) {
        VoiceMemoDialog(
            recorder = voiceRecorder,
            onDismiss = {
                voiceRecorder.cancel()
                showVoiceDialog = false
            },
            onSend = { file, durationMs ->
                showVoiceDialog = false
                sendVoiceFile(file, durationMs)
            },
        )
    }
}

private fun launchCamera(context: android.content.Context, onReady: (Uri) -> Unit) {
    val dir = File(context.cacheDir, "camera").apply { mkdirs() }
    val file = File(dir, "capture-${System.currentTimeMillis()}.jpg")
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
    onReady(uri)
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
    attachMenuOpen: Boolean = false,
    onAttachMenuChange: (Boolean) -> Unit = {},
    onPickGallery: () -> Unit = {},
    onPickCamera: () -> Unit = {},
    onPickVoice: () -> Unit = {},
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
    val visibleMessages = remember(messages) { messages.filter { isVisibleChatKind(it.kind) } }
    val gaps = remember(visibleMessages) {
        val result = mutableSetOf<Int>()
        val lastLamport = mutableMapOf<String, ULong>()
        visibleMessages.forEachIndexed { index, msg ->
            val senderHex = formatUserId(msg.senderUserId)
            val previous = lastLamport[senderHex]
            if (previous != null && msg.lamport > previous + 1uL) {
                result.add(index)
            }
            lastLamport[senderHex] = maxOf(previous ?: 0uL, msg.lamport)
        }
        result
    }
    val grouping = remember(visibleMessages) {
        val meta = visibleMessages.map { ConversationMessageMeta(formatUserId(it.senderUserId), it.timestamp) }
        meta.indices.map { bubbleGroupingFor(meta, it) }
    }
    var showContactDetails by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }

    LaunchedEffect(visibleMessages.size) {
        if (visibleMessages.isNotEmpty()) {
            listState.animateScrollToItem(visibleMessages.size - 1)
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
                    visibleMessages,
                    key = { _, message -> "${message.senderUserId.contentHashCode()}:${message.lamport}" },
                ) { index, message ->
                    val isOwn = message.senderUserId.contentEquals(ownUserId)

                    if (isNewDay(visibleMessages, index)) {
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
                Box {
                    IconButton(
                        onClick = { onAttachMenuChange(true) },
                        modifier = Modifier.semantics {
                            contentDescription = "Attach photo or voice memo"
                        },
                    ) {
                        Icon(Icons.Default.Add, contentDescription = null)
                    }
                    DropdownMenu(
                        expanded = attachMenuOpen,
                        onDismissRequest = { onAttachMenuChange(false) },
                    ) {
                        DropdownMenuItem(
                            text = { Text("Photo library") },
                            onClick = onPickGallery,
                        )
                        DropdownMenuItem(
                            text = { Text("Take photo") },
                            onClick = onPickCamera,
                        )
                        DropdownMenuItem(
                            text = { Text("Voice memo") },
                            onClick = onPickVoice,
                        )
                    }
                }
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

@Composable
private fun VoiceMemoDialog(
    recorder: VoiceRecorder,
    onDismiss: () -> Unit,
    onSend: (File, Int) -> Unit,
) {
    var recording by remember { mutableStateOf(false) }
    var elapsedMs by remember { mutableLongStateOf(0L) }
    var error by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(recording) {
        if (!recording) return@LaunchedEffect
        val start = System.currentTimeMillis()
        while (recording) {
            elapsedMs = System.currentTimeMillis() - start
            if (elapsedMs >= MAX_VOICE_MS) {
                val result = recorder.stop()
                recording = false
                if (result != null) {
                    onSend(result.first, result.second)
                } else {
                    error = "Recording failed"
                }
                return@LaunchedEffect
            }
            delay(200)
        }
    }

    AlertDialog(
        onDismissRequest = {
            recorder.cancel()
            onDismiss()
        },
        title = { Text("Voice memo") },
        text = {
            Column {
                Text(
                    if (recording) {
                        "Recording… ${formatDurationMs(elapsedMs.toInt())} / ${formatDurationMs(MAX_VOICE_MS)}"
                    } else {
                        "Tap Start, then Send when you're done (max 60s)."
                    },
                )
                if (error != null) {
                    Text(
                        text = error!!,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
        },
        confirmButton = {
            if (recording) {
                TextButton(
                    onClick = {
                        val result = recorder.stop()
                        recording = false
                        if (result != null) {
                            onSend(result.first, result.second)
                        } else {
                            error = "Recording failed"
                        }
                    },
                ) { Text("Send") }
            } else {
                TextButton(
                    onClick = {
                        error = null
                        if (recorder.start()) {
                            recording = true
                            elapsedMs = 0L
                        } else {
                            error = "Could not access microphone"
                        }
                    },
                ) { Text("Start") }
            }
        },
        dismissButton = {
            TextButton(
                onClick = {
                    recorder.cancel()
                    onDismiss()
                },
            ) { Text("Cancel") }
        },
    )
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
                Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
                    when (message.kind) {
                        KIND_ATTACHMENT_MANIFEST -> {
                            val attachment = remember(message.payload) {
                                AttachmentPayload.decode(message.payload)
                            }
                            if (attachment == null) {
                                Text("Unsupported attachment")
                            } else {
                                AttachmentBubbleContent(attachment = attachment, contentColor = contentColor)
                            }
                        }
                        else -> {
                            Text(message.payload.toString(Charsets.UTF_8))
                        }
                    }
                    if (tick != null) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(top = 4.dp),
                            horizontalArrangement = Arrangement.End,
                        ) {
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

@Composable
private fun AttachmentBubbleContent(
    attachment: AttachmentPayload,
    contentColor: Color,
) {
    when (attachment.mediaType) {
        AttachmentPayload.MediaType.IMAGE -> {
            val bitmap = remember(attachment.blob) {
                BitmapFactory.decodeByteArray(attachment.blob, 0, attachment.blob.size)?.asImageBitmap()
            }
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = "Photo",
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 240.dp),
                )
            } else {
                Text("Photo (could not display)")
            }
            if (attachment.caption.isNotBlank()) {
                Text(
                    text = attachment.caption,
                    modifier = Modifier.padding(top = 6.dp),
                )
            }
        }
        AttachmentPayload.MediaType.AUDIO -> {
            VoiceMemoPlayer(
                blob = attachment.blob,
                durationMs = attachment.durationMs,
                contentColor = contentColor,
            )
            if (attachment.caption.isNotBlank()) {
                Text(
                    text = attachment.caption,
                    modifier = Modifier.padding(top = 6.dp),
                )
            }
        }
    }
}

@Composable
private fun VoiceMemoPlayer(
    blob: ByteArray,
    durationMs: Int,
    contentColor: Color,
) {
    val context = LocalContext.current
    var playing by remember { mutableStateOf(false) }
    var player by remember { mutableStateOf<MediaPlayer?>(null) }

    DisposableEffect(blob) {
        onDispose {
            player?.release()
            player = null
            playing = false
        }
    }

    Row(verticalAlignment = Alignment.CenterVertically) {
        IconButton(
            onClick = {
                if (playing) {
                    player?.stop()
                    player?.release()
                    player = null
                    playing = false
                } else {
                    try {
                        val temp = File(context.cacheDir, "play-${System.currentTimeMillis()}.m4a")
                        temp.writeBytes(blob)
                        val mp = MediaPlayer().apply {
                            setDataSource(temp.absolutePath)
                            setOnCompletionListener {
                                playing = false
                                release()
                                player = null
                                temp.delete()
                            }
                            prepare()
                            start()
                        }
                        player = mp
                        playing = true
                    } catch (_: Exception) {
                        playing = false
                        Toast.makeText(context, "Could not play voice memo", Toast.LENGTH_SHORT).show()
                    }
                }
            },
            modifier = Modifier.size(40.dp),
        ) {
            Icon(
                imageVector = if (playing) Icons.Default.Close else Icons.Default.PlayArrow,
                contentDescription = if (playing) "Stop" else "Play voice memo",
                tint = contentColor,
            )
        }
        Text(
            text = "Voice memo · ${formatDurationMs(durationMs)}",
            style = MaterialTheme.typography.bodyMedium,
        )
    }
}

private fun formatDurationMs(ms: Int): String {
    val totalSec = ((ms + 500) / 1000).coerceAtLeast(0)
    val min = totalSec / 60
    val sec = totalSec % 60
    return "%d:%02d".format(min, sec)
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
