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
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.input.pointer.pointerInput
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
import com.cruisemesh.app.ui.ComposerCameraIcon
import com.cruisemesh.app.ui.ComposerMicIcon
import com.cruisemesh.app.ui.ComposerSendIcon
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

/** Below this hold duration a mic press is treated as an accidental tap, not a memo. */
private const val MIN_VOICE_MS = 500L

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
    var pendingCameraUri by remember { mutableStateOf<Uri?>(null) }
    // A photo picked but not yet sent: shown as a preview card above the composer
    // so a caption can ride along with it in a single attachment (see [onSend]).
    var pendingPhoto by remember { mutableStateOf<ByteArray?>(null) }
    val voiceRecorder = remember { VoiceRecorder(context) }

    fun reload() {
        messages = store.messagesForChat(contact.userId)
        deliveredThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_DELIVERED)
        readThrough = store.receiptThrough(contact.userId, ownUserId, RECEIPT_TYPE_READ)
    }

    fun stagePhoto(jpeg: ByteArray?) {
        if (jpeg == null) {
            Toast.makeText(context, "Could not prepare photo (too large or unreadable)", Toast.LENGTH_SHORT).show()
            return
        }
        pendingPhoto = jpeg
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
            stagePhoto(MediaCompressor.compressImageUri(context, uri))
        }
    }

    val cameraLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { success ->
        val uri = pendingCameraUri
        pendingCameraUri = null
        if (success && uri != null) {
            stagePhoto(MediaCompressor.compressImageUri(context, uri))
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
            Toast.makeText(context, "Microphone ready — hold the mic to record", Toast.LENGTH_SHORT).show()
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
        pendingPhoto = pendingPhoto,
        onClearPendingPhoto = { pendingPhoto = null },
        onSend = {
            val text = draft.trim()
            val photo = pendingPhoto
            if (photo != null) {
                sender.sendAttachment(
                    contact,
                    AttachmentPayload(
                        mediaType = AttachmentPayload.MediaType.IMAGE,
                        mimeType = "image/jpeg",
                        durationMs = 0,
                        blob = photo,
                        caption = text,
                    ),
                )
                pendingPhoto = null
                draft = ""
                reload()
            } else if (text.isNotEmpty()) {
                sender.sendText(contact, text)
                draft = ""
                reload()
            }
        },
        onPickGallery = {
            galleryLauncher.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
        },
        onPickCamera = {
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
        onStartVoice = {
            val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED
            if (granted) {
                voiceRecorder.start()
            } else {
                micPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                false
            }
        },
        onStopVoice = {
            val result = voiceRecorder.stop()
            if (result != null) {
                sendVoiceFile(result.first, result.second)
            } else {
                Toast.makeText(context, "Recording failed", Toast.LENGTH_SHORT).show()
            }
        },
        onCancelVoice = { voiceRecorder.cancel() },
        onBack = onBack,
        onDeleteContact = onDeleteContact,
    )
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
    pendingPhoto: ByteArray? = null,
    onClearPendingPhoto: () -> Unit = {},
    onPickGallery: () -> Unit = {},
    onPickCamera: () -> Unit = {},
    onStartVoice: () -> Boolean = { false },
    onStopVoice: () -> Unit = {},
    onCancelVoice: () -> Unit = {},
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

            if (pendingPhoto != null) {
                PendingPhotoCard(bytes = pendingPhoto, onRemove = onClearPendingPhoto)
            }

            MessageComposer(
                draft = draft,
                onDraftChange = onDraftChange,
                onSend = onSend,
                hasPendingAttachment = pendingPhoto != null,
                ownBubbleColor = MaterialTheme.colorScheme.primary,
                onPickGallery = onPickGallery,
                onPickCamera = onPickCamera,
                onStartVoice = onStartVoice,
                onStopVoice = onStopVoice,
                onCancelVoice = onCancelVoice,
            )
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

/**
 * Preview card for a photo that's been picked but not yet sent. Shown just
 * above the composer with a remove button, so the user can type a caption that
 * rides along with the image in a single attachment.
 */
@Composable
private fun PendingPhotoCard(bytes: ByteArray, onRemove: () -> Unit) {
    val bitmap = remember(bytes) {
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp),
    ) {
        Box {
            if (bitmap != null) {
                Image(
                    bitmap = bitmap,
                    contentDescription = "Photo to send",
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .size(72.dp)
                        .clip(RoundedCornerShape(12.dp)),
                )
            }
            Box(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(4.dp)
                    .size(22.dp)
                    .clip(CircleShape)
                    .background(Color.Black.copy(alpha = 0.55f))
                    .clickable(onClick = onRemove)
                    .semantics { contentDescription = "Remove photo" },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Default.Close,
                    contentDescription = null,
                    tint = Color.White,
                    modifier = Modifier.size(16.dp),
                )
            }
        }
        Spacer(modifier = Modifier.width(12.dp))
        Text(
            text = "Photo ready — add a caption or send",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * Signal-style message composer: a circular "+" (in the user's own-bubble
 * color) that opens the photo library, a rounded input pill with a camera
 * icon inside on the right, and a trailing action that is a hold-to-record
 * microphone when the draft is empty and a send button once there's text.
 *
 * Voice memos record while the mic is held and send on release (a press
 * shorter than [MIN_VOICE_MS] is treated as an accidental tap and discarded).
 * The recorder itself is owned by [ChatScreen]; this composable only drives it
 * through [onStartVoice] / [onStopVoice] / [onCancelVoice].
 */
@Composable
private fun MessageComposer(
    draft: String,
    onDraftChange: (String) -> Unit,
    onSend: () -> Unit,
    hasPendingAttachment: Boolean,
    ownBubbleColor: Color,
    onPickGallery: () -> Unit,
    onPickCamera: () -> Unit,
    onStartVoice: () -> Boolean,
    onStopVoice: () -> Unit,
    onCancelVoice: () -> Unit,
) {
    val context = LocalContext.current
    val onBubbleColor = MaterialTheme.colorScheme.onPrimary
    var recording by remember { mutableStateOf(false) }
    var elapsedMs by remember { mutableLongStateOf(0L) }
    // A staged photo can be sent on its own, so the send button shows whenever
    // there's text *or* a pending attachment; the mic only takes over when the
    // composer is otherwise empty.
    val canSend = draft.isNotBlank() || hasPendingAttachment

    LaunchedEffect(recording) {
        if (!recording) return@LaunchedEffect
        val start = System.currentTimeMillis()
        while (recording) {
            elapsedMs = System.currentTimeMillis() - start
            if (elapsedMs >= MAX_VOICE_MS) {
                recording = false
                onStopVoice()
                return@LaunchedEffect
            }
            delay(100)
        }
    }

    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 16.dp),
    ) {
        Box(
            modifier = Modifier
                .size(44.dp)
                .clip(CircleShape)
                .background(ownBubbleColor)
                .clickable(onClick = onPickGallery)
                .semantics { contentDescription = "Attach photo from library" },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.Default.Add, contentDescription = null, tint = onBubbleColor)
        }

        Spacer(modifier = Modifier.width(8.dp))

        if (recording) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .weight(1f)
                    .clip(RoundedCornerShape(24.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant)
                    .padding(horizontal = 16.dp, vertical = 14.dp),
            ) {
                Box(
                    modifier = Modifier
                        .size(10.dp)
                        .clip(CircleShape)
                        .background(Color(0xFFE53935)),
                )
                Spacer(modifier = Modifier.width(10.dp))
                Text("Recording… ${formatDurationMs(elapsedMs.toInt())}")
                Spacer(modifier = Modifier.weight(1f))
                Text(
                    text = "release to send",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            TextField(
                value = draft,
                onValueChange = onDraftChange,
                placeholder = { Text(if (hasPendingAttachment) "Add a caption…" else "Message") },
                trailingIcon = {
                    IconButton(onClick = onPickCamera) {
                        Icon(ComposerCameraIcon, contentDescription = "Take photo")
                    }
                },
                shape = RoundedCornerShape(24.dp),
                maxLines = 5,
                colors = TextFieldDefaults.colors(
                    focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                    unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                    focusedIndicatorColor = Color.Transparent,
                    unfocusedIndicatorColor = Color.Transparent,
                    disabledIndicatorColor = Color.Transparent,
                ),
                modifier = Modifier.weight(1f),
            )
        }

        Spacer(modifier = Modifier.width(8.dp))

        if (canSend && !recording) {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .background(ownBubbleColor)
                    .clickable(onClick = onSend)
                    .semantics { contentDescription = "Send" },
                contentAlignment = Alignment.Center,
            ) {
                Icon(ComposerSendIcon, contentDescription = null, tint = onBubbleColor)
            }
        } else {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .background(if (recording) ownBubbleColor else Color.Transparent)
                    .pointerInput(Unit) {
                        detectTapGestures(
                            onPress = {
                                val started = onStartVoice()
                                if (!started) {
                                    tryAwaitRelease()
                                    return@detectTapGestures
                                }
                                recording = true
                                elapsedMs = 0L
                                val pressStart = System.currentTimeMillis()
                                tryAwaitRelease()
                                val held = System.currentTimeMillis() - pressStart
                                if (recording) {
                                    recording = false
                                    if (held < MIN_VOICE_MS) {
                                        onCancelVoice()
                                        Toast.makeText(
                                            context,
                                            "Hold the mic to record a voice memo",
                                            Toast.LENGTH_SHORT,
                                        ).show()
                                    } else {
                                        onStopVoice()
                                    }
                                }
                            },
                        )
                    }
                    .semantics { contentDescription = "Hold to record a voice memo" },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    ComposerMicIcon,
                    contentDescription = null,
                    tint = if (recording) onBubbleColor else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
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
