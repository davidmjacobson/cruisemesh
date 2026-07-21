package com.cruisemesh.app.chat

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.media.KIND_GROUP_INVITE
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.MediaCompressor
import com.cruisemesh.app.media.VoiceRecorder
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.notify.ChatMuteStore
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.ui.AvatarBadge
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ConversationMessageMeta
import com.cruisemesh.app.ui.bubbleGroupingFor
import com.cruisemesh.app.ui.formatConversationTimestamp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.formatUserId
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.core.content.ContextCompat
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.res.pluralStringResource
import com.cruisemesh.app.R

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
    memberReachabilityByUserId: Map<String, ReachabilityLevel> = emptyMap(),
) {
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    val density = LocalDensity.current
    val keyboardFreeze = rememberOverlayKeyboardFreeze()
    var currentGroup by remember(group.id) { mutableStateOf(group) }
    var messages by remember(group.id) { mutableStateOf(store.messagesForChat(group.id)) }
    var draft by remember(group.id) { mutableStateOf(DraftStore.load(context, group.id)) }
    var pendingPhoto by remember { mutableStateOf<ByteArray?>(null) }
    var isMuted by remember(group.id) { mutableStateOf(ChatMuteStore.isMuted(context, group.id)) }
    var pendingCameraUri by remember { mutableStateOf<Uri?>(null) }
    var showDetails by remember { mutableStateOf(false) }
    var showRename by remember { mutableStateOf(false) }
    var renameDraft by remember(group.id) { mutableStateOf(group.name) }
    var showAddMembers by remember { mutableStateOf(false) }
    var selectedAddMemberIds by remember { mutableStateOf(setOf<String>()) }
    var groupActionError by remember { mutableStateOf<String?>(null) }
    var confirmDelete by remember { mutableStateOf(false) }
    var focused by remember(group.id) { mutableStateOf<FocusedMessage?>(null) }
    var infoMessage by remember(group.id) { mutableStateOf<StoredMessage?>(null) }
    var replyingTo by remember(group.id) { mutableStateOf<StoredMessage?>(null) }
    val snackbarHostState = remember { SnackbarHostState() }
    val coroutineScope = rememberCoroutineScope()
    val voiceRecorder = remember { VoiceRecorder(context) }

    fun reload() {
        messages = store.messagesForChat(currentGroup.id)
        store.getGroup(currentGroup.id)?.let { currentGroup = it }
    }

    fun showSendFailure() {
        coroutineScope.launch {
            snackbarHostState.showSnackbar("Couldn't send. Your message is still here.")
        }
    }

    fun stagePhoto(jpeg: ByteArray?) {
        if (jpeg == null) {
            Toast.makeText(context, "Could not prepare photo (too large or unreadable)", Toast.LENGTH_SHORT).show()
        } else {
            pendingPhoto = jpeg
        }
    }

    fun sendVoiceFile(file: java.io.File, durationMs: Int) {
        val bytes = try { file.readBytes() } catch (_: Exception) { null }
        file.delete()
        if (bytes == null || bytes.isEmpty()) {
            Toast.makeText(context, "Could not save voice memo", Toast.LENGTH_SHORT).show()
            return
        }
        if (bytes.size > AttachmentPayload.MAX_BLOB_BYTES) {
            Toast.makeText(context, "Voice memo is too large to send over the mesh", Toast.LENGTH_SHORT).show()
            return
        }
        val replyId = replyingTo?.let {
            store.messageReference(it.chatId, it.senderUserId, it.lamport)?.msgId
        }
        if (sender.sendAttachment(
                currentGroup,
                AttachmentPayload(
                    mediaType = AttachmentPayload.MediaType.AUDIO,
                    mimeType = "audio/mp4",
                    durationMs = durationMs.coerceAtMost(MAX_VOICE_MS),
                    blob = bytes,
                ),
                replyId,
            ) == SendResult.STORED
        ) {
            replyingTo = null
            reload()
        } else {
            showSendFailure()
        }
    }

    val galleryLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri -> if (uri != null) stagePhoto(MediaCompressor.compressImageUri(context, uri)) }

    val cameraLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { success ->
        val uri = pendingCameraUri
        pendingCameraUri = null
        if (success && uri != null) stagePhoto(MediaCompressor.compressImageUri(context, uri))
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
        Toast.makeText(
            context,
            if (granted) "Microphone ready — hold the mic to record" else "Microphone permission is required for voice memos",
            Toast.LENGTH_SHORT,
        ).show()
    }

    DisposableEffect(Unit) { onDispose { voiceRecorder.cancel() } }

    fun senderName(userId: ByteArray): String {
        if (userId.contentEquals(ownUserId)) return "You"
        val contact = contactsByUserId[UserIdHex.encode(userId)]
        return contact?.let(::coreContactDisplayName)?.takeIf { it.isNotBlank() }
            ?: formatUserId(userId)
    }

    LaunchedEffect(group.id) {
        ChatEvents.changes.collect { changedChatId ->
            if (changedChatId.contentEquals(group.id)) {
                reload()
            }
        }
    }

    LaunchedEffect(draft) {
        DraftStore.save(context, group.id, draft)
    }

    val listState = rememberLazyListState()
    val scrollScope = rememberCoroutineScope()
    val visibleMessages = remember(messages) { messages.filter { isVisibleChatKind(it.kind) } }
    // FA4: same off-main-thread load as ChatScreen -- reply-quote metadata and
    // own-message expiry watermarks, queried once per visible-list change
    // instead of during composition/recomposition.
    val chatExtras by produceState(ChatExtras(), visibleMessages, ownUserId, contactsByUserId) {
        value = withContext(Dispatchers.IO) {
            loadChatExtras(store, visibleMessages, ownUserId) { message -> senderName(message.senderUserId) }
        }
    }
    val replyMetadata = chatExtras.replyMetadata
    val replyingToPreview = remember(replyingTo, ownUserId, contactsByUserId) {
        replyingTo?.let { target ->
            quotedMessagePreview(target) { message -> senderName(message.senderUserId) }
        }
    }
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
        sender.sendReaction(currentGroup, target, if (existingOwn != null) "" else emoji)
        reload()
    }

    fun scrollToMessage(message: StoredMessage) {
        val oldestFirstIndex = visibleMessages.indexOfFirst { messageStableKey(it) == messageStableKey(message) }
        if (oldestFirstIndex < 0) return
        val displayIndex = visibleMessages.lastIndex - oldestFirstIndex
        scrollScope.launch { listState.animateScrollToItem(displayIndex) }
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

    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
    val viewportHeightPx = with(density) { maxHeight.toPx() }
    Scaffold(
        topBar = {
            GroupConversationTopBar(
                group = currentGroup,
                memberCount = currentGroup.memberUserIds.size,
                reachableMemberCount = reachableMemberCount,
                onBack = onBack,
                onOpenDetails = { showDetails = true },
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { innerPadding ->
        // This device uses adjustResize, so the viewport already excludes the
        // IME. Track its usable bottom edge rather than adding IME padding a
        // second time; OverlayKeyboardFreeze pins that edge while the keyboard
        // animates away and back.
        val bottomInsetPx = with(density) { innerPadding.calculateBottomPadding().toPx() }
        val contentBottomPx = viewportHeightPx - bottomInsetPx
        val imeVisible = WindowInsets.ime.getBottom(density) > 0
        SideEffect { keyboardFreeze.trackLiveContentBottom(contentBottomPx, imeVisible) }
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
                        groupName = currentGroup.name,
                        grouping = grouping[index],
                        quoted = replyMetadata[messageStableKey(message)]?.quoted,
                        onQuotedClick = { target -> scrollToMessage(target) },
                        reactions = reactions[MessageTarget(message.senderUserId, message.lamport, message.kind).stableKey].orEmpty(),
                        onReact = { emoji ->
                            toggleReaction(MessageTarget(message.senderUserId, message.lamport, message.kind), emoji)
                        },
                        onLongPress = { target, bounds -> openOverlay(target, bounds) },
                    )
                }
            }

            if (replyingToPreview != null) {
                ReplyComposerPreview(
                    preview = replyingToPreview,
                    onCancel = { replyingTo = null },
                    modifier = Modifier.padding(bottom = 8.dp),
                )
            }

            pendingPhoto?.let { photo ->
                PendingPhotoCard(bytes = photo, onRemove = { pendingPhoto = null })
            }
            MessageComposer(
                draft = draft,
                onDraftChange = { draft = it },
                onSend = {
                    val text = draft.trim()
                    val replyToMsgId = replyingTo?.let { replyMetadata[messageStableKey(it)]?.msgId }
                    val photo = pendingPhoto
                    val result = if (photo != null) {
                        sender.sendAttachment(
                            currentGroup,
                            AttachmentPayload(
                                mediaType = AttachmentPayload.MediaType.IMAGE,
                                mimeType = "image/jpeg",
                                durationMs = 0,
                                blob = photo,
                                caption = text,
                            ),
                            replyToMsgId,
                        )
                    } else if (text.isNotEmpty()) {
                        sender.sendText(currentGroup, text, replyToMsgId)
                    } else {
                        SendResult.FAILED
                    }
                    if (result == SendResult.STORED) {
                        pendingPhoto = null
                        draft = ""
                        replyingTo = null
                        reload()
                    } else {
                        showSendFailure()
                    }
                },
                hasPendingAttachment = pendingPhoto != null,
                ownBubbleColor = MaterialTheme.colorScheme.primary,
                onPickGallery = {
                    galleryLauncher.launch(
                        PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                    )
                },
                onPickCamera = {
                    if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                        PackageManager.PERMISSION_GRANTED
                    ) {
                        launchCamera(context) { uri ->
                            pendingCameraUri = uri
                            cameraLauncher.launch(uri)
                        }
                    } else {
                        cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
                    }
                },
                onStartVoice = {
                    if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) !=
                        PackageManager.PERMISSION_GRANTED
                    ) {
                        micPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                        false
                    } else {
                        voiceRecorder.start()
                    }
                },
                onStopVoice = {
                    voiceRecorder.stop()?.let { (file, durationMs) -> sendVoiceFile(file, durationMs) }
                },
                onCancelVoice = { voiceRecorder.cancel() },
            )
        }
    }

    if (showDetails) {
        AlertDialog(
            onDismissRequest = { showDetails = false },
            title = { Text(currentGroup.name) },
            text = {
                Column {
                    Text(
                        pluralStringResource(
                            R.plurals.ui_member_count,
                            currentGroup.memberUserIds.size,
                            currentGroup.memberUserIds.size,
                        ),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(bottom = 8.dp),
                    )
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(stringResource(R.string.ui_mute_notifications), modifier = Modifier.weight(1f))
                        Switch(
                            checked = isMuted,
                            onCheckedChange = {
                                isMuted = it
                                ChatMuteStore.setMuted(context, currentGroup.id, it)
                                ChatEvents.notifyChatChanged(currentGroup.id)
                            },
                        )
                    }
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        TextButton(
                            onClick = {
                                renameDraft = currentGroup.name
                                groupActionError = null
                                showDetails = false
                                showRename = true
                            },
                        ) { Text(stringResource(R.string.ui_rename)) }
                        TextButton(
                            onClick = {
                                selectedAddMemberIds = emptySet()
                                groupActionError = null
                                showDetails = false
                                showAddMembers = true
                            },
                            enabled = contactsByUserId.values.any { contact ->
                                currentGroup.memberUserIds.none { it.contentEquals(contact.userId) }
                            },
                        ) { Text(stringResource(R.string.ui_add_members)) }
                    }
                    for (memberId in currentGroup.memberUserIds) {
                        val memberKey = UserIdHex.encode(memberId)
                        val memberName = if (memberId.contentEquals(ownUserId)) {
                            "You"
                        } else {
                            senderName(memberId)
                        }
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(vertical = 5.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            AvatarBadge(
                                userId = memberId,
                                name = memberName,
                                displayId = memberKey,
                                size = 36.dp,
                                reachability = if (memberId.contentEquals(ownUserId)) {
                                    null
                                } else {
                                    memberReachabilityByUserId[memberKey]
                                },
                            )
                            Spacer(modifier = Modifier.width(10.dp))
                            Text(memberName, style = MaterialTheme.typography.bodyMedium)
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        showDetails = false
                        confirmDelete = true
                    },
                ) { Text(stringResource(R.string.ui_leave_delete)) }
            },
            dismissButton = {
                TextButton(onClick = { showDetails = false }) { Text(stringResource(R.string.ui_close)) }
            },
        )
    }

    if (showRename) {
        AlertDialog(
            onDismissRequest = { showRename = false },
            title = { Text(stringResource(R.string.ui_rename_group)) },
            text = {
                Column {
                    OutlinedTextField(
                        value = renameDraft,
                        onValueChange = { renameDraft = it },
                        label = { Text(stringResource(R.string.ui_group_name)) },
                        singleLine = true,
                    )
                    groupActionError?.let {
                        Text(
                            it,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(
                    enabled = renameDraft.trim().isNotEmpty() && renameDraft.trim() != currentGroup.name,
                    onClick = {
                        val updated = sender.renameGroup(currentGroup, renameDraft)
                        if (updated == null) {
                            groupActionError = "Couldn't rename the group. The change was not queued."
                        } else {
                            currentGroup = updated
                            showRename = false
                            showDetails = true
                        }
                    },
                ) { Text(stringResource(R.string.ui_rename)) }
            },
            dismissButton = {
                TextButton(onClick = { showRename = false }) { Text(stringResource(R.string.ui_cancel)) }
            },
        )
    }

    if (showAddMembers) {
        val availableContacts = contactsByUserId.values
            .filter { contact -> currentGroup.memberUserIds.none { it.contentEquals(contact.userId) } }
            .sortedBy { it.name.lowercase() }
        AlertDialog(
            onDismissRequest = { showAddMembers = false },
            title = { Text(stringResource(R.string.ui_add_members)) },
            text = {
                Column {
                    if (availableContacts.isEmpty()) {
                        Text(stringResource(R.string.ui_all_of_your_contacts_are_already_in_this))
                    } else {
                        for (contact in availableContacts) {
                            val key = UserIdHex.encode(contact.userId)
                            val selected = key in selectedAddMemberIds
                            Row(
                                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Checkbox(
                                    checked = selected,
                                    onCheckedChange = { checked ->
                                        selectedAddMemberIds = if (checked) {
                                            selectedAddMemberIds + key
                                        } else {
                                            selectedAddMemberIds - key
                                        }
                                    },
                                )
                                AvatarBadge(
                                    userId = contact.userId,
                                    name = coreContactDisplayName(contact),
                                    displayId = key,
                                    size = 36.dp,
                                    reachability = memberReachabilityByUserId[key],
                                )
                                Spacer(modifier = Modifier.width(10.dp))
                                Text(ChatListLogic.displayNameOrId(coreContactDisplayName(contact), key))
                            }
                        }
                    }
                    groupActionError?.let {
                        Text(
                            it,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(
                    enabled = selectedAddMemberIds.isNotEmpty(),
                    onClick = {
                        val additions = availableContacts.filter {
                            UserIdHex.encode(it.userId) in selectedAddMemberIds
                        }
                        val updated = sender.addMembers(currentGroup, additions)
                        if (updated == null) {
                            groupActionError = "Couldn't add members. No invitations were queued."
                        } else {
                            currentGroup = updated
                            showAddMembers = false
                            showDetails = true
                        }
                    },
                ) { Text(stringResource(R.string.ui_add_count, selectedAddMemberIds.size)) }
            },
            dismissButton = {
                TextButton(onClick = { showAddMembers = false }) { Text(stringResource(R.string.ui_cancel)) }
            },
        )
    }

    if (confirmDelete) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text(stringResource(R.string.ui_delete_named, currentGroup.name)) },
            text = {
                Text(stringResource(R.string.ui_removes_this_group_and_its_message_history_from))
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmDelete = false
                        onDeleteGroup()
                    },
                ) { Text(stringResource(R.string.ui_delete)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = false }) { Text(stringResource(R.string.ui_cancel)) }
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
            val focusedReplyMetadata = replyMetadata[messageStableKey(focusedMessage)]

            MessageFocusOverlay(
                focused = currentFocused,
                isOwn = focusedIsOwn,
                canReply = focusedReplyMetadata?.msgId != null,
                canCopy = focusedCopyText.isNotBlank(),
                ownReactionEmoji = focusedOwnReaction,
                onDismiss = { closeOverlay() },
                onReact = { emoji ->
                    toggleReaction(currentFocused.target, emoji)
                    closeOverlay()
                },
                onReply = {
                    replyingTo = focusedMessage
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
                    quoted = focusedReplyMetadata?.quoted,
                )
            }
        }
    }
    } // Box

    val currentInfoMessage = infoMessage
    if (currentInfoMessage != null) {
        val infoIsOwn = currentInfoMessage.senderUserId.contentEquals(ownUserId)
        val infoArrival = if (infoIsOwn) {
            null
        } else {
            store.messageArrival(
                currentInfoMessage.chatId,
                currentInfoMessage.senderUserId,
                currentInfoMessage.lamport,
            )
        }
        MessageInfoBottomSheet(
            onDismiss = { infoMessage = null },
            text = messageInfoText(
                currentInfoMessage,
                infoIsOwn,
                null,
                infoArrival,
                outboundExpiryMs = if (infoIsOwn) {
                    chatExtras.outboundExpiryMs[messageStableKey(currentInfoMessage)]
                } else {
                    null
                },
                nowMs = System.currentTimeMillis(),
            ),
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
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
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
                    isGroup = true,
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
            TextButton(onClick = onOpenDetails) { Text(stringResource(R.string.ui_info)) }
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
    quoted: QuotedMessagePreview? = null,
    onQuotedClick: (StoredMessage) -> Unit = {},
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

    var boundsInWindow by remember { mutableStateOf(Rect.Zero) }
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
            quoted = quoted,
            onQuotedClick = quoted?.target?.let { target -> { onQuotedClick(target) } },
            modifier = Modifier
                .onGloballyPositioned { coords -> boundsInWindow = coords.boundsInWindow() }
                .messageActions(
                    onLongClick = { onLongPress(target, boundsInWindow) },
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
    quoted: QuotedMessagePreview? = null,
    onQuotedClick: (() -> Unit)? = null,
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
                if (quoted != null) {
                    QuotedMessageBlock(
                        preview = quoted,
                        accentColor = if (isOwn) contentColor else MaterialTheme.colorScheme.primary,
                        contentColor = contentColor,
                        onClick = onQuotedClick,
                        modifier = Modifier.padding(bottom = 8.dp),
                    )
                }
                if (message.kind == KIND_ATTACHMENT_MANIFEST) {
                    val attachment = remember(message.payload) {
                        AttachmentPayload.decode(message.payload)
                    }
                    if (attachment == null) {
                        Text(stringResource(R.string.ui_unsupported_attachment))
                    } else {
                        AttachmentBubbleContent(attachment, contentColor)
                    }
                } else {
                    Text(
                        text = String(message.payload, Charsets.UTF_8),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
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
