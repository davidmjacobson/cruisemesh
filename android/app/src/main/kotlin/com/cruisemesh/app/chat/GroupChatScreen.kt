package com.cruisemesh.app.chat

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.itemsIndexed
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
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.media.KIND_GROUP_INVITE
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.ImageGallery
import com.cruisemesh.app.media.MediaCompressor
import com.cruisemesh.app.media.VoiceRecorder
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.notify.ChatMuteStore
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.ui.AvatarBadge
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ConversationMessageMeta
import com.cruisemesh.app.ui.SignalTick
import com.cruisemesh.app.ui.bubbleGroupingFor
import com.cruisemesh.app.ui.formatConversationTimestamp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.GroupReceiptState
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.coreGroupTickStatusFor
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.formatUserId
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.core.content.ContextCompat
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.res.pluralStringResource
import com.cruisemesh.app.R

/**
 * Group chat thread (DESIGN.md §6.5). Local `chat_id` is the group id.
 * Own-message ticks come from per-member D9 watermarks in [GroupReceiptState].
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
    var currentGroup by remember(group.id) { mutableStateOf(group) }
    var messages by remember(group.id) { mutableStateOf(store.messagesForChat(group.id)) }
    var receiptState by remember(group.id) {
        mutableStateOf(store.groupReceiptState(group.id, ownUserId, group.memberUserIds))
    }
    var draft by remember(group.id) { mutableStateOf(DraftStore.load(context, group.id)) }
    var pendingPhoto by remember { mutableStateOf<ByteArray?>(null) }
    var viewerPhoto by remember(group.id) { mutableStateOf<ByteArray?>(null) }
    var reactionDetails by remember(group.id) { mutableStateOf<GroupReactionDetails?>(null) }
    // The staged photo currently open in the markup editor, or null when it is
    // closed (specs/photo-markup.md).
    var drawingPhoto by remember { mutableStateOf<ByteArray?>(null) }
    var isMuted by remember(group.id) { mutableStateOf(ChatMuteStore.isMuted(context, group.id)) }
    var pendingCameraUri by remember { mutableStateOf<Uri?>(null) }
    var showDetails by remember { mutableStateOf(false) }
    var showRename by remember { mutableStateOf(false) }
    var renameDraft by remember(group.id) { mutableStateOf(group.name) }
    var showAddMembers by remember { mutableStateOf(false) }
    var selectedAddMemberIds by remember { mutableStateOf(setOf<String>()) }
    var groupActionError by remember { mutableStateOf<String?>(null) }
    var confirmDelete by remember { mutableStateOf(false) }
    var replyingTo by remember(group.id) { mutableStateOf<StoredMessage?>(null) }
    val snackbarHostState = remember { SnackbarHostState() }
    val coroutineScope = rememberCoroutineScope()
    val voiceRecorder = remember { VoiceRecorder(context) }

    fun reload() {
        messages = store.messagesForChat(currentGroup.id)
        store.getGroup(currentGroup.id)?.let { currentGroup = it }
        receiptState = store.groupReceiptState(currentGroup.id, ownUserId, currentGroup.memberUserIds)
    }

    fun tickFor(message: StoredMessage): TickStatus? {
        if (!message.senderUserId.contentEquals(ownUserId)) return null
        return when (coreGroupTickStatusFor(message.lamport, message.timestamp, ownUserId, receiptState)) {
            uniffi.cruisemesh_core.CoreTickStatus.READ -> TickStatus.READ
            uniffi.cruisemesh_core.CoreTickStatus.DELIVERED -> TickStatus.DELIVERED
            uniffi.cruisemesh_core.CoreTickStatus.SENT -> TickStatus.SENT
        }
    }

    fun showSendFailure() = showSendFailureSnackbar(coroutineScope, snackbarHostState)

    fun stagePhoto(jpeg: ByteArray?) = stagePhotoOrWarn(context, jpeg) { pendingPhoto = it }

    fun sendVoiceFile(file: java.io.File, durationMs: Int) {
        val bytes = readVoiceMemoBytes(context, file) ?: return
        val replyId = replyingTo?.let {
            store.messageReference(it.chatId, it.senderUserId, it.lamport)?.msgId
        }
        if (sender.sendAttachment(
                currentGroup,
                AttachmentPayload(
                    mediaType = AttachmentPayload.MediaType.AUDIO,
                    mimeType = VoiceRecorder.plan.mimeType,
                    durationMs = durationMs,
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
            Toast.makeText(
                context,
                context.getString(R.string.ui_camera_permission_required_for_photos),
                Toast.LENGTH_SHORT,
            ).show()
        }
    }

    val micPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        Toast.makeText(
            context,
            context.getString(
                if (granted) R.string.ui_microphone_ready else R.string.ui_microphone_permission_needed,
            ),
            Toast.LENGTH_SHORT,
        ).show()
    }

    DisposableEffect(Unit) { onDispose { voiceRecorder.cancel() } }

    fun senderName(userId: ByteArray): String {
        if (userId.contentEquals(ownUserId)) return context.getString(R.string.ui_you)
        val contact = contactsByUserId[UserIdHex.encode(userId)]
        return contact?.let(::coreContactDisplayName)?.takeIf { it.isNotBlank() }
            ?: ChatListLogic.unknownGroupMemberLabel(
                formatUserId(userId),
                context.getString(R.string.ui_unknown_group_member),
            )
    }

    fun showReactionDetails(target: MessageTarget, emoji: String) {
        val members = reactorUserIdsForReaction(messages, target, emoji).map { userId ->
            GroupReactionMember(userId, senderName(userId))
        }.sortedWith(
            compareBy<GroupReactionMember> { !it.userId.contentEquals(ownUserId) }
                .thenBy(String.CASE_INSENSITIVE_ORDER) { it.name }
                .thenBy { UserIdHex.encode(it.userId) },
        )
        reactionDetails = GroupReactionDetails(emoji, members)
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

    val host = rememberConversationHost(group.id)
    val linkHandler = rememberMessageLinkHandler()
    val visibleMessages = remember(messages) { messages.filter { isVisibleChatKind(it.kind) } }
    // FA4: same off-main-thread load as ChatScreen -- reply-quote metadata and
    // own-message expiry watermarks, queried once per visible-list change
    // instead of during composition/recomposition.
    var chatExtras by remember(visibleMessages, ownUserId, contactsByUserId) {
        mutableStateOf(ChatExtras())
    }
    LaunchedEffect(visibleMessages, ownUserId, contactsByUserId) {
        chatExtras = withContext(Dispatchers.IO) {
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
        sender.sendReaction(currentGroup, target, resolveReactionToggle(reactions, target, emoji))
        reload()
    }

    fun scrollToMessage(message: StoredMessage) = host.scrollToMessage(visibleMessages, message)

    // The overlay takes over the full screen, so drop the keyboard while it's
    // open and bring it back once closed. OverlayKeyboardFreeze keeps the
    // conversation pixel-frozen while the keyboard animates, Signal-style.
    fun openOverlay(target: MessageTarget, bounds: Rect) = host.openOverlay(target, bounds)

    fun closeOverlay() = host.closeOverlay()

    ConversationHostEffects(host, visibleMessages, ownUserId, chatExtras.lateArrivalMs.keys)

    ConversationScaffold(
        host = host,
        topBar = {
            GroupConversationTopBar(
                group = currentGroup,
                memberCount = currentGroup.memberUserIds.size,
                reachableMemberCount = reachableMemberCount,
                onBack = onBack,
                onOpenDetails = { showDetails = true },
            )
        },
        snackbarHostState = snackbarHostState,
        listContent = {
            itemsIndexed(
                displayMessages,
                key = { _, message -> messageItemKey(message) },
            ) { revIndex, message ->
                val index = visibleMessages.lastIndex - revIndex
                val isOwn = message.senderUserId.contentEquals(ownUserId)
                GroupMessageBubble(
                    message = message,
                    tick = tickFor(message),
                    isFocused = host.focused?.target == MessageTarget(
                        message.senderUserId,
                        message.lamport,
                        message.kind,
                    ),
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
                        showReactionDetails(MessageTarget(message.senderUserId, message.lamport, message.kind), emoji)
                    },
                    onPhotoClick = { viewerPhoto = it },
                    lateArrivalMs = chatExtras.lateArrivalMs[messageStableKey(message)],
                    onLongPress = { target, bounds -> openOverlay(target, bounds) },
                    onLinkClick = { link -> linkHandler.open(link) },
                )
            }
        },
        belowList = {
            if (replyingToPreview != null) {
                ReplyComposerPreview(
                    preview = replyingToPreview,
                    onCancel = { replyingTo = null },
                    modifier = Modifier.padding(bottom = 8.dp),
                )
            }

            pendingPhoto?.let { photo ->
                PendingPhotoCard(
                    bytes = photo,
                    onRemove = { pendingPhoto = null },
                    onDraw = { drawingPhoto = photo },
                )
            }
            MessageComposer(
                draft = draft,
                onDraftChange = { draft = it },
                onSend = {
                    val replyToMsgId = replyingTo?.let { replyMetadata[messageStableKey(it)]?.msgId }
                    val outcome = ComposerSendPolicy.attempt(
                        draft = draft,
                        pendingPhoto = pendingPhoto,
                        sendPhoto = { photo, caption ->
                            sender.sendAttachment(
                                currentGroup,
                                AttachmentPayload(
                                    mediaType = AttachmentPayload.MediaType.IMAGE,
                                    mimeType = "image/jpeg",
                                    durationMs = 0,
                                    blob = photo,
                                    caption = caption,
                                ),
                                replyToMsgId,
                            )
                        },
                        sendText = { text -> sender.sendText(currentGroup, text, replyToMsgId) },
                    )
                    // Assigned back unconditionally: the composer empties only
                    // when [ComposerSendPolicy] says the message is durably queued.
                    draft = outcome.draft
                    pendingPhoto = outcome.pendingPhoto
                    when (outcome.status) {
                        ComposerSendStatus.QUEUED -> {
                            replyingTo = null
                            reload()
                        }
                        ComposerSendStatus.NOT_QUEUED -> showSendFailure()
                        ComposerSendStatus.NOTHING_TO_SEND -> Unit
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
                    // stop() drains the recorder's buffered tail off the UI thread,
                    // then delivers the finalized file back on the main thread.
                    voiceRecorder.stop { result ->
                        result?.let { (file, durationMs) -> sendVoiceFile(file, durationMs) }
                    }
                },
                onCancelVoice = { voiceRecorder.cancel() },
                bytesRecorded = { voiceRecorder.bytesRecorded() },
            )
        },
        overlays = {
            MessageLinkPrompt(linkHandler)

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
                        val memberContactName = contactsByUserId[memberKey]
                            ?.let(::coreContactDisplayName)
                            ?.takeIf { it.isNotBlank() }
                        val isUnknownMember = !memberId.contentEquals(ownUserId) && memberContactName == null
                        val memberName = if (memberId.contentEquals(ownUserId)) {
                            "You"
                        } else {
                            memberContactName ?: ChatListLogic.unknownGroupMemberLabel(
                                formatUserId(memberId),
                                context.getString(R.string.ui_unknown_group_member),
                            )
                        }
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(vertical = 5.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            AvatarBadge(
                                userId = memberId,
                                name = if (isUnknownMember) "" else memberName,
                                displayId = memberKey,
                                size = 36.dp,
                                reachability = if (memberId.contentEquals(ownUserId)) {
                                    null
                                } else {
                                    memberReachabilityByUserId[memberKey]
                                },
                            )
                            Spacer(modifier = Modifier.width(10.dp))
                            Column {
                                Text(memberName, style = MaterialTheme.typography.bodyMedium)
                                if (isUnknownMember) {
                                    Text(
                                        formatUserId(memberId),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
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
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .toggleable(
                                        value = selected,
                                        role = Role.Checkbox,
                                        onValueChange = { checked ->
                                            selectedAddMemberIds = if (checked) {
                                                selectedAddMemberIds + key
                                            } else {
                                                selectedAddMemberIds - key
                                            }
                                        },
                                    )
                                    .padding(vertical = 4.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Checkbox(
                                    checked = selected,
                                    onCheckedChange = null,
                                    modifier = Modifier.size(48.dp),
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

    val currentFocused = host.focused
    if (currentFocused != null) {
        val focusedMessage = host.resolveFocusedMessage(visibleMessages)
        // focusedMessage is null only if the message vanished from under us
        // (e.g. deleted) while the overlay was open; just render nothing.
        if (focusedMessage != null) {
            val focusedIsOwn = focusedMessage.senderUserId.contentEquals(ownUserId)
            val focusedIndex = visibleMessages.indexOf(focusedMessage)
            val focusedGrouping = grouping.getOrNull(focusedIndex) ?: BubbleGrouping(joinsPrevious = false, joinsNext = false)
            val focusedShape = bubbleShapeFor(focusedIsOwn, focusedGrouping)
            val focusedReactions = reactions[currentFocused.target.stableKey].orEmpty()
            val focusedCopyText = remember(focusedMessage.payload, focusedMessage.kind) { messageCopyText(focusedMessage) }
            val focusedImage = remember(focusedMessage.payload, focusedMessage.kind) { messageImageBytes(focusedMessage) }
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
                        Toast.makeText(context, R.string.ui_copied, Toast.LENGTH_SHORT).show()
                    }
                    closeOverlay()
                },
                onSaveImage = focusedImage?.let { jpeg ->
                    {
                        val saved = ImageGallery.saveJpeg(context, jpeg)
                        Toast.makeText(
                            context,
                            context.getString(
                                if (saved != null) {
                                    R.string.ui_saved_to_pictures
                                } else {
                                    R.string.ui_could_not_save_image
                                },
                            ),
                            Toast.LENGTH_SHORT,
                        ).show()
                        closeOverlay()
                    }
                },
                onInfo = {
                    host.openInfo(focusedMessage)
                },
            ) {
                GroupMessageBubbleVisual(
                    message = focusedMessage,
                    isOwn = focusedIsOwn,
                    tick = tickFor(focusedMessage),
                    senderLabel = focusedSenderLabel,
                    shape = focusedShape,
                    showTimestamp = focusedGrouping.showTimestamp,
                    reactions = focusedReactions,
                    onReact = { emoji ->
                        closeOverlay()
                        showReactionDetails(currentFocused.target, emoji)
                    },
                    quoted = focusedReplyMetadata?.quoted,
                )
            }
        }
    }
        },
    )

    val currentInfoMessage = host.infoMessage
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
            onDismiss = { host.closeInfo() },
            rows = groupMessageInfoRows(
                currentInfoMessage,
                infoIsOwn,
                tickFor(currentInfoMessage),
                infoArrival,
                receiptState,
                ownUserId,
                senderName = { senderName(it) },
                outboundExpiryMs = if (infoIsOwn) {
                    chatExtras.outboundExpiryMs[messageStableKey(currentInfoMessage)]
                } else {
                    null
                },
                senderDisplayId = if (infoIsOwn) null else formatUserId(currentInfoMessage.senderUserId),
                senderIdLabel = if (infoIsOwn) null else context.getString(R.string.ui_sender_id),
                nowMs = System.currentTimeMillis(),
            ),
        )
    }

    val currentViewerPhoto = viewerPhoto
    if (currentViewerPhoto != null) {
        PhotoViewerOverlay(
            jpeg = currentViewerPhoto,
            onDismiss = { viewerPhoto = null },
        )
    }

    val currentReactionDetails = reactionDetails
    if (currentReactionDetails != null) {
        GroupReactionDetailsSheet(
            details = currentReactionDetails,
            onDismiss = { reactionDetails = null },
        )
    }

    // Placed at the screen's outer level so it covers the whole conversation
    // rather than nesting inside a composer slot.
    val photoBeingDrawnOn = drawingPhoto
    if (photoBeingDrawnOn != null) {
        PhotoMarkupEditor(
            jpeg = photoBeingDrawnOn,
            onCancel = { drawingPhoto = null },
            onConfirm = { annotated ->
                drawingPhoto = null
                // Same staging path as a freshly picked photo, so the caption
                // and reply target already in the composer are untouched.
                stagePhoto(annotated)
            },
        )
    }
}

private class GroupReactionMember(
    val userId: ByteArray,
    val name: String,
)

private class GroupReactionDetails(
    val emoji: String,
    val members: List<GroupReactionMember>,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GroupReactionDetailsSheet(
    details: GroupReactionDetails,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(details.emoji, style = MaterialTheme.typography.titleLarge)
                Text(stringResource(R.string.ui_reactions), style = MaterialTheme.typography.titleLarge)
            }
            for (member in details.members) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 16.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    AvatarBadge(
                        userId = member.userId,
                        name = member.name,
                        displayId = formatUserId(member.userId),
                        size = 36.dp,
                    )
                    Spacer(modifier = Modifier.width(12.dp))
                    Text(member.name, style = MaterialTheme.typography.bodyLarge)
                }
            }
        }
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
    // T8: same treatment as ConversationTopBar -- the group's name + photo
    // already live in Scaffold's topBar slot (pinned above the message
    // LazyColumn), so a persistent elevation just reinforces visually that
    // they stay above the scrolling conversation.
    Surface(tonalElevation = 2.dp, shadowElevation = 2.dp) {
        TopAppBar(
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
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
}

@Composable
internal fun GroupMessageBubble(
    message: StoredMessage,
    tick: TickStatus?,
    isFocused: Boolean,
    isOwn: Boolean,
    senderLabel: String?,
    groupName: String,
    grouping: BubbleGrouping,
    quoted: QuotedMessagePreview? = null,
    onQuotedClick: (StoredMessage) -> Unit = {},
    reactions: List<ReactionSummary> = emptyList(),
    onReact: (String) -> Unit = {},
    onPhotoClick: (ByteArray) -> Unit = {},
    /** When this message reached this device, if its place in the thread needs explaining. */
    lateArrivalMs: Long? = null,
    onLongPress: (MessageTarget, Rect) -> Unit = { _, _ -> },
    onLinkClick: (MessageLink) -> Unit = {},
) {
    val context = LocalContext.current
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
    val photoBytes = remember(message.kind, message.payload) { messageImageBytes(message) }
    val onBubbleClick: () -> Unit = {
        if (photoBytes != null) onPhotoClick(photoBytes)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = topPadding, bottom = bottomPadding),
        horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start,
    ) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
    ) {
        GroupMessageBubbleVisual(
            message = message,
            isOwn = isOwn,
            tick = tick,
            senderLabel = senderLabel,
            shape = shape,
            showTimestamp = grouping.showTimestamp,
            reactions = reactions,
            onReact = onReact,
            quoted = quoted,
            onQuotedClick = quoted?.target?.let { target -> { onQuotedClick(target) } },
            // The body text handles taps on its own glyphs (a link needs the
            // tap position) and hands the long-press straight back, so the
            // reaction/copy overlay still opens from anywhere in the bubble.
            bodyActions = MessageBodyActions(
                onLinkClick = onLinkClick,
                onClick = onBubbleClick,
                onLongClick = { onLongPress(target, boundsInRoot) },
            ),
            modifier = Modifier
                .alpha(if (isFocused) 0f else 1f)
                .onGloballyPositioned { coords -> boundsInRoot = coords.unclippedBoundsInRoot() }
                .messageActions(
                    onClick = onBubbleClick,
                    onLongClick = { onLongPress(target, boundsInRoot) },
                ),
        )
    }
        // Set only for a message spliced in above content already here --
        // see core/src/late_arrival.rs. The bubble keeps the sender's time.
        if (lateArrivalMs != null) {
            Text(
                text = stringResource(
                    R.string.ui_arrived_at,
                    formatConversationTimestamp(context, lateArrivalMs),
                ),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 12.dp),
            )
        }
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
    tick: TickStatus? = null,
    senderLabel: String?,
    shape: RoundedCornerShape,
    showTimestamp: Boolean,
    reactions: List<ReactionSummary>,
    onReact: (String) -> Unit,
    modifier: Modifier = Modifier,
    quoted: QuotedMessagePreview? = null,
    onQuotedClick: (() -> Unit)? = null,
    bodyActions: MessageBodyActions? = null,
) {
    val context = LocalContext.current
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
                        AttachmentBubbleContent(
                            attachment = attachment,
                            messageKey = messageItemKey(message),
                            contentColor = contentColor,
                            isOwn = isOwn,
                            bodyActions = bodyActions,
                        )
                    }
                } else {
                    MessageBodyText(
                        body = String(message.payload, Charsets.UTF_8),
                        isOwn = isOwn,
                        style = MaterialTheme.typography.bodyLarge,
                        actions = bodyActions,
                    )
                }
                if (showTimestamp || isOwn) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .align(Alignment.End)
                            .padding(top = 4.dp),
                    ) {
                        if (showTimestamp) {
                            Text(
                                text = formatConversationTimestamp(context, message.timestamp),
                                style = MaterialTheme.typography.labelSmall,
                                color = contentColor.copy(alpha = 0.7f),
                            )
                        }
                        if (isOwn && tick != null) {
                            val tickBaseColor =
                                if (bubbleColor.luminance() > 0.5f) Color.Black else Color.White
                            val tint = when (tick) {
                                TickStatus.SENT -> tickBaseColor.copy(alpha = 0.88f)
                                TickStatus.DELIVERED -> tickBaseColor.copy(alpha = 0.74f)
                                TickStatus.READ -> tickBaseColor
                            }
                            SignalTick(
                                status = tick,
                                tint = tint,
                                bubbleColor = bubbleColor,
                                modifier = Modifier.padding(start = 6.dp),
                            )
                        }
                    }
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

internal fun groupMessageInfoRows(
    message: StoredMessage,
    isOwn: Boolean,
    tick: TickStatus?,
    arrival: uniffi.cruisemesh_core.MessageArrival?,
    receiptState: GroupReceiptState,
    ownUserId: ByteArray,
    senderName: (ByteArray) -> String,
    outboundExpiryMs: Long? = null,
    senderDisplayId: String? = null,
    senderIdLabel: String? = null,
    nowMs: Long = System.currentTimeMillis(),
): List<MessageInfoRow> {
    val rows = messageInfoRows(
        message,
        isOwn,
        tick,
        arrival,
        outboundExpiryMs = outboundExpiryMs,
        senderDisplayId = senderDisplayId,
        senderIdLabel = senderIdLabel,
        nowMs = nowMs,
    ).toMutableList()
    if (!isOwn) return rows
    for (member in receiptState.members) {
        if (member.memberUserId.contentEquals(ownUserId)) continue
        if (member.addedAtMs > 0L && member.addedAtMs > message.timestamp) continue
        val memberTick = tickStatusFor(message.lamport, member.deliveredThrough, member.readThrough)
        val status = when (memberTick) {
            TickStatus.READ -> "Read"
            TickStatus.DELIVERED -> "Delivered"
            TickStatus.SENT -> "Waiting"
        }
        val route = member.deliveredViaTransport?.let { transport ->
            " · ${transportRouteText(transport.toInt())}"
        }.orEmpty()
        rows.add(MessageInfoRow.LabelValue(senderName(member.memberUserId), status + route))
    }
    return rows
}
