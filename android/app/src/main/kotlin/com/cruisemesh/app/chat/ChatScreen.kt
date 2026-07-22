package com.cruisemesh.app.chat

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.media.MediaPlayer
import android.net.Uri
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.spring
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarHostState
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
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.core.content.ContextCompat
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.ChatImageDecoder
import com.cruisemesh.app.media.ImageGallery
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.MediaCompressor
import com.cruisemesh.app.media.VoiceRecorder
import com.cruisemesh.app.media.createCameraCaptureUri
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.mesh.ContactReachability
import com.cruisemesh.app.notify.ChatMuteStore
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.ui.AvatarBadge
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ComposerCameraIcon
import com.cruisemesh.app.ui.ComposerMicIcon
import com.cruisemesh.app.ui.ComposerSendIcon
import com.cruisemesh.app.ui.ReplyIcon
import com.cruisemesh.app.ui.ContactDetailsSheet
import com.cruisemesh.app.ui.ConversationMessageMeta
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.SignalTick
import com.cruisemesh.app.ui.bubbleGroupingFor
import com.cruisemesh.app.ui.formatConversationTimestamp
import com.cruisemesh.app.ui.tickLegendText
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.formatUserId
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlin.math.roundToInt
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

/** The `kind` byte for a plaintext chat message (DESIGN.md §7.1). */
private const val KIND_TEXT: kotlin.UByte = 1u

/** `receipt_type` values (DESIGN.md §7.2), for reading own-message tick watermarks out of the store. */
private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u

internal const val MAX_VOICE_MS = 60_000

/** Below this hold duration a mic press is treated as an accidental tap, not a memo. */
internal const val MIN_VOICE_MS = 500L

val REACTION_CHOICES = listOf("👍", "❤️", "😂", "😮", "😢", "🙏")

/**
 * A single 1:1 chat thread (DESIGN.md §7.1: for a 1:1 chat, `chat_id` is
 * simply the peer's UserID). Renders visible chat kinds (text + attachment
 * manifests) in a bottom-anchored list (newest just above the composer /
 * keyboard via [LazyColumn] `reverseLayout`), with the local user's
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
    reachability: ReachabilityLevel = ReachabilityLevel.OFFLINE,
    reachabilityStatusText: String = ContactReachability.chatHeaderCopy(ReachabilityLevel.OFFLINE, null, 0L),
    reachabilityDetailsText: String = reachabilityStatusText,
) {
    val context = LocalContext.current
    var currentContact by remember(contact.userId) { mutableStateOf(contact) }
    var messages by remember(contact.userId) { mutableStateOf(store.messagesForChat(currentContact.userId)) }
    var contactAvatar by remember(contact.userId) { mutableStateOf(store.contactAvatar(currentContact.userId)) }
    var deliveredThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(currentContact.userId, ownUserId, RECEIPT_TYPE_DELIVERED))
    }
    var readThrough by remember(contact.userId) {
        mutableStateOf(store.receiptThrough(currentContact.userId, ownUserId, RECEIPT_TYPE_READ))
    }
    // T6: the transport a delivery receipt last returned on, for the Info pane.
    var deliveredVia by remember(contact.userId) {
        mutableStateOf(store.receiptViaTransport(currentContact.userId, ownUserId, RECEIPT_TYPE_DELIVERED))
    }
    var draft by remember(contact.userId) { mutableStateOf(DraftStore.load(context, contact.userId)) }
    var isMuted by remember(contact.userId) { mutableStateOf(ChatMuteStore.isMuted(context, contact.userId)) }
    var replyingTo by remember(contact.userId) { mutableStateOf<StoredMessage?>(null) }
    var pendingCameraUri by remember { mutableStateOf<Uri?>(null) }
    // A photo picked but not yet sent: shown as a preview card above the composer
    // so a caption can ride along with it in a single attachment (see [onSend]).
    var pendingPhoto by remember { mutableStateOf<ByteArray?>(null) }
    val snackbarHostState = remember { SnackbarHostState() }
    val coroutineScope = rememberCoroutineScope()
    val voiceRecorder = remember { VoiceRecorder(context) }

    fun replyTargetId(message: StoredMessage): ByteArray? =
        store.messageReference(message.chatId, message.senderUserId, message.lamport)?.msgId

    fun reload() {
        currentContact = store.getContact(contact.userId) ?: currentContact
        messages = store.messagesForChat(currentContact.userId)
        contactAvatar = store.contactAvatar(currentContact.userId)
        deliveredThrough = store.receiptThrough(currentContact.userId, ownUserId, RECEIPT_TYPE_DELIVERED)
        readThrough = store.receiptThrough(currentContact.userId, ownUserId, RECEIPT_TYPE_READ)
        deliveredVia = store.receiptViaTransport(currentContact.userId, ownUserId, RECEIPT_TYPE_DELIVERED)
    }

    fun stagePhoto(jpeg: ByteArray?) = stagePhotoOrWarn(context, jpeg) { pendingPhoto = it }

    fun showSendFailure(message: String = SEND_FAILURE_MESSAGE) =
        showSendFailureSnackbar(coroutineScope, snackbarHostState, message)

    fun sendVoiceFile(file: File, durationMs: Int) {
        val bytes = readVoiceMemoBytes(context, file) ?: return
        val result = sender.sendAttachment(
            currentContact,
            AttachmentPayload(
                mediaType = AttachmentPayload.MediaType.AUDIO,
                mimeType = "audio/mp4",
                durationMs = durationMs.coerceAtMost(MAX_VOICE_MS),
                blob = bytes,
            ),
            replyingTo?.let(::replyTargetId),
        )
        if (result == SendResult.STORED) {
            replyingTo = null
            reload()
        } else {
            // The recording file is already gone, so the generic "still here"
            // copy would be wrong for a voice memo.
            showSendFailure("Couldn't send the voice memo. Try recording it again.")
        }
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
            if (changedChatId.contentEquals(currentContact.userId)) {
                reload()
            }
        }
    }

    LaunchedEffect(draft) {
        DraftStore.save(context, currentContact.userId, draft)
    }

    ConversationScreen(
        contact = currentContact,
        ownUserId = ownUserId,
        messages = messages,
        store = store,
        contactAvatar = contactAvatar,
        deliveredThrough = deliveredThrough,
        readThrough = readThrough,
        deliveredVia = deliveredVia,
        replyingTo = replyingTo,
        onReplyingToChange = { replyingTo = it },
        arrivalFor = { message ->
            store.messageArrival(message.chatId, message.senderUserId, message.lamport)
        },
        snackbarHostState = snackbarHostState,
        draft = draft,
        onDraftChange = { draft = it },
        pendingPhoto = pendingPhoto,
        onClearPendingPhoto = { pendingPhoto = null },
        onSend = {
            val text = draft.trim()
            val photo = pendingPhoto
            val replyToMsgId = replyingTo?.let(::replyTargetId)
            if (photo != null) {
                val result = sender.sendAttachment(
                    currentContact,
                    AttachmentPayload(
                        mediaType = AttachmentPayload.MediaType.IMAGE,
                        mimeType = "image/jpeg",
                        durationMs = 0,
                        blob = photo,
                        caption = text,
                    ),
                    replyToMsgId,
                )
                if (result == SendResult.STORED) {
                    pendingPhoto = null
                    draft = ""
                    replyingTo = null
                    reload()
                } else {
                    showSendFailure()
                }
            } else if (text.isNotEmpty()) {
                if (sender.sendText(currentContact, text, replyToMsgId) == SendResult.STORED) {
                    draft = ""
                    replyingTo = null
                    reload()
                } else {
                    showSendFailure()
                }
            }
        },
        onReact = { target, emoji ->
            sender.sendReaction(currentContact, target, emoji)
            reload()
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
        reachability = reachability,
        reachabilityStatusText = reachabilityStatusText,
        reachabilityDetailsText = reachabilityDetailsText,
        isMuted = isMuted,
        onMutedChange = {
            isMuted = it
            ChatMuteStore.setMuted(context, currentContact.userId, it)
            ChatEvents.notifyChatChanged(currentContact.userId)
        },
        onSetNickname = { nickname ->
            store.setContactNickname(currentContact.userId, nickname)
            reload()
            ChatEvents.notifyChatChanged(currentContact.userId)
        },
    )
}

internal fun launchCamera(context: android.content.Context, onReady: (Uri) -> Unit) {
    onReady(createCameraCaptureUri(context))
}

@Composable
private fun ConversationScreen(
    contact: Contact,
    ownUserId: ByteArray,
    messages: List<StoredMessage>,
    store: MessageStore? = null,
    contactAvatar: ByteArray? = null,
    deliveredThrough: ULong,
    readThrough: ULong,
    deliveredVia: UByte? = null,
    replyingTo: StoredMessage? = null,
    onReplyingToChange: (StoredMessage?) -> Unit = {},
    arrivalFor: (StoredMessage) -> MessageArrival? = { null },
    snackbarHostState: SnackbarHostState,
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
    onReact: (MessageTarget, String) -> Unit = { _, _ -> },
    onBack: () -> Unit,
    onDeleteContact: () -> Unit,
    reachability: ReachabilityLevel = ReachabilityLevel.OFFLINE,
    reachabilityStatusText: String = ContactReachability.chatHeaderCopy(ReachabilityLevel.OFFLINE, null, 0L),
    reachabilityDetailsText: String = reachabilityStatusText,
    isMuted: Boolean = false,
    onMutedChange: (Boolean) -> Unit = {},
    onSetNickname: (String?) -> Unit = {},
) {
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    val composerFocus = remember { FocusRequester() }
    // Swipe-to-reply (T1): start a reply to [message] and open the keyboard.
    fun startReply(message: StoredMessage) {
        onReplyingToChange(message)
        composerFocus.requestFocus()
    }
    val host = rememberConversationHost(contact.userId)
    val displayId = remember(contact.userId) { formatUserId(contact.userId) }
    val resolvedName = remember(contact.name, contact.nickname) {
        coreContactDisplayName(contact)
    }
    val displayName = remember(resolvedName, displayId) {
        ChatListLogic.displayNameOrId(resolvedName, displayId)
    }
    val (contactColor, _) = remember(contact, resolvedName, displayId) {
        ChatListLogic.avatarHueAndInitials(contact.userId, resolvedName, displayId)
    }
    val visibleMessages = remember(messages) { messages.filter { isVisibleChatKind(it.kind) } }
    // FA4: reply-quote metadata and outbound-expiry watermarks are per-message
    // store reads; load them off the main thread whenever the visible list
    // changes instead of querying the store during composition (see
    // ChatExtras' item-lambda / info-sheet lookups below).
    val chatExtras by produceState(ChatExtras(), visibleMessages, ownUserId, displayName, store) {
        value = if (store == null) {
            ChatExtras()
        } else {
            withContext(Dispatchers.IO) {
                loadChatExtras(store, visibleMessages, ownUserId) { message ->
                    if (message.senderUserId.contentEquals(ownUserId)) "You" else displayName
                }
            }
        }
    }
    val replyMetadata = chatExtras.replyMetadata
    val replyingToPreview = remember(replyingTo, ownUserId, displayName) {
        replyingTo?.let { target ->
            quotedMessagePreview(target) { message ->
                if (message.senderUserId.contentEquals(ownUserId)) "You" else displayName
            }
        }
    }
    val gaps = remember(messages, visibleMessages) { visibleGapIndices(messages, visibleMessages) }
    val reactions = remember(messages, ownUserId) { reactionSummariesByTarget(messages, ownUserId) }
    val grouping = remember(visibleMessages) {
        val meta = visibleMessages.map { ConversationMessageMeta(formatUserId(it.senderUserId), it.timestamp) }
        meta.indices.map { bubbleGroupingFor(meta, it) }
    }
    var showContactDetails by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }
    var viewerPhoto by remember(contact.userId) { mutableStateOf<ByteArray?>(null) }
    // Newest-first for reverseLayout LazyColumn: index 0 sits at the bottom
    // edge (just above the composer / keyboard), empty space stays above.
    val displayMessages = remember(visibleMessages) { visibleMessages.asReversed() }

    fun toggleReaction(target: MessageTarget, emoji: String) =
        onReact(target, resolveReactionToggle(reactions, target, emoji))

    fun scrollToMessage(message: StoredMessage) = host.scrollToMessage(visibleMessages, message)

    // The overlay takes over the full screen, so drop the keyboard while it's
    // open and bring it back once closed. OverlayKeyboardFreeze keeps the
    // conversation pixel-frozen while the keyboard animates, Signal-style.
    fun openOverlay(target: MessageTarget, bounds: Rect) = host.openOverlay(target, bounds)

    fun closeOverlay() = host.closeOverlay()

    ConversationHostEffects(host, visibleMessages, ownUserId)

    ConversationScaffold(
        host = host,
        topBar = {
            ConversationTopBar(
                contact = contact,
                displayId = displayId,
                displayName = displayName,
                statusText = reachabilityStatusText,
                reachability = reachability,
                avatarBytes = contactAvatar,
                onBack = onBack,
                onOpenDetails = { showContactDetails = true },
            )
        },
        snackbarHostState = snackbarHostState,
        listContent = {
            itemsIndexed(
                displayMessages,
                key = { _, message -> messageItemKey(message) },
            ) { revIndex, message ->
                // Map back to oldest-first index for gap / day / grouping logic.
                val index = visibleMessages.lastIndex - revIndex
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
                    quoted = replyMetadata[messageStableKey(message)]?.quoted,
                    onQuotedClick = { target -> scrollToMessage(target) },
                    reactions = reactions[MessageTarget(message.senderUserId, message.lamport, message.kind).stableKey].orEmpty(),
                    onReact = { emoji ->
                        toggleReaction(MessageTarget(message.senderUserId, message.lamport, message.kind), emoji)
                    },
                    onPhotoClick = { viewerPhoto = it },
                    outboundExpiryMs = if (isOwn) chatExtras.outboundExpiryMs[messageStableKey(message)] else null,
                    onLongPress = { target, bounds -> openOverlay(target, bounds) },
                    onSwipeReply = { startReply(message) },
                )
            }
        },
        belowList = {
            if (pendingPhoto != null) {
                PendingPhotoCard(bytes = pendingPhoto, onRemove = onClearPendingPhoto)
            }

            if (replyingToPreview != null) {
                ReplyComposerPreview(
                    preview = replyingToPreview,
                    onCancel = { onReplyingToChange(null) },
                    modifier = Modifier.padding(bottom = 8.dp),
                )
            }

            MessageComposer(
                draft = draft,
                onDraftChange = onDraftChange,
                onSend = onSend,
                hasPendingAttachment = pendingPhoto != null,
                ownBubbleColor = MaterialTheme.colorScheme.primary,
                focusRequester = composerFocus,
                onPickGallery = onPickGallery,
                onPickCamera = onPickCamera,
                onStartVoice = onStartVoice,
                onStopVoice = onStopVoice,
                onCancelVoice = onCancelVoice,
            )
        },
        overlays = {
            if (showContactDetails) {
                ContactDetailsSheet(
                    contact = contact,
                    connectivityText = reachabilityDetailsText,
                    isMuted = isMuted,
                    onMutedChange = onMutedChange,
                    onSetNickname = onSetNickname,
                    avatarBytes = contactAvatar,
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
                    title = { Text(stringResource(R.string.ui_delete_named, displayName)) },
                    text = { Text(stringResource(R.string.ui_this_removes_the_contact_and_deletes_your_chat)) },
                    confirmButton = {
                        TextButton(
                            onClick = {
                                confirmDelete = false
                                onDeleteContact()
                            },
                        ) {
                            Text(stringResource(R.string.ui_delete))
                        }
                    },
                    dismissButton = {
                        TextButton(onClick = { confirmDelete = false }) {
                            Text(stringResource(R.string.ui_cancel))
                        }
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
                    val focusedTick = if (focusedIsOwn) tickStatusFor(focusedMessage.lamport, deliveredThrough, readThrough) else null
                    val focusedReactions = reactions[currentFocused.target.stableKey].orEmpty()
                    val focusedCopyText = remember(focusedMessage.payload, focusedMessage.kind) { messageCopyText(focusedMessage) }
                    val focusedOwnReaction = focusedReactions.firstOrNull { it.reactedByOwnUser }?.emoji
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
                            onReplyingToChange(focusedMessage)
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
                            host.infoMessage = focusedMessage
                            closeOverlay()
                        },
                    ) {
                        MessageBubbleVisual(
                            message = focusedMessage,
                            isOwn = focusedIsOwn,
                            tick = focusedTick,
                            contactColor = if (focusedIsOwn) null else contactColor,
                            shape = focusedShape,
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
        },
    )

    val currentInfoMessage = host.infoMessage
    if (currentInfoMessage != null) {
        val infoIsOwn = currentInfoMessage.senderUserId.contentEquals(ownUserId)
        val infoTick = if (infoIsOwn) tickStatusFor(currentInfoMessage.lamport, deliveredThrough, readThrough) else null
        val infoArrival = if (infoIsOwn) {
            null
        } else {
            arrivalFor(currentInfoMessage)
        }
        // T6: an own message delivered through the current watermark shows the
        // route the confirmation returned on (covers every acked message, not
        // just the one at the exact watermark lamport).
        val deliveredViaRoute = deliveredVia
            ?.takeIf { infoIsOwn && currentInfoMessage.lamport <= deliveredThrough }
            ?.let { transportRouteText(it.toInt()) }
        MessageInfoBottomSheet(
            onDismiss = { host.infoMessage = null },
            rows = messageInfoRows(
                    currentInfoMessage,
                    infoIsOwn,
                    infoTick,
                    infoArrival,
                    deliveredViaRoute = deliveredViaRoute,
                    outboundExpiryMs = if (infoIsOwn) {
                        chatExtras.outboundExpiryMs[messageStableKey(currentInfoMessage)]
                    } else {
                        null
                    },
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
}

/**
 * Preview card for a photo that's been picked but not yet sent. Shown just
 * above the composer with a remove button, so the user can type a caption that
 * rides along with the image in a single attachment.
 */
@Composable
internal fun PendingPhotoCard(bytes: ByteArray, onRemove: () -> Unit) {
    val density = LocalDensity.current
    val previewPx = with(density) { 72.dp.toPx().roundToInt() }
    val bitmap by produceState<ImageBitmap?>(null, bytes, previewPx) {
        value = withContext(Dispatchers.IO) {
            ChatImageDecoder.decodeSampled(bytes, previewPx, previewPx)?.asImageBitmap()
        }
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp),
    ) {
        Box {
            val currentBitmap = bitmap
            if (currentBitmap != null) {
                Image(
                    bitmap = currentBitmap,
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
                    // FA10: 22dp visually, but the touch target itself grows
                    // to the 48dp minimum via the invisible padding this adds.
                    .minimumInteractiveComponentSize()
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
        Text(text = stringResource(R.string.ui_photo_ready_add_a_caption_or_send),
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
internal fun MessageComposer(
    draft: String,
    onDraftChange: (String) -> Unit,
    onSend: () -> Unit,
    hasPendingAttachment: Boolean,
    ownBubbleColor: Color,
    focusRequester: FocusRequester = remember { FocusRequester() },
    onPickGallery: () -> Unit,
    onPickCamera: () -> Unit,
    onStartVoice: () -> Boolean,
    onStopVoice: () -> Unit,
    onCancelVoice: () -> Unit,
) {
    val context = LocalContext.current
    val haptic = LocalHapticFeedback.current
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
                // FA10: 44dp visually; minimumInteractiveComponentSize() pads
                // the touch target up to the 48dp minimum.
                .minimumInteractiveComponentSize()
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
                Text(stringResource(R.string.ui_recording_duration, formatDurationMs(elapsedMs.toInt())))
                Spacer(modifier = Modifier.weight(1f))
                Text(text = stringResource(R.string.ui_release_to_send),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            TextField(
                value = draft,
                onValueChange = onDraftChange,
                placeholder = {
                    Text(stringResource(if (hasPendingAttachment) R.string.ui_add_a_caption else R.string.ui_message))
                },
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
                modifier = Modifier
                    .weight(1f)
                    .focusRequester(focusRequester),
            )
        }

        Spacer(modifier = Modifier.width(8.dp))

        if (canSend && !recording) {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .background(ownBubbleColor)
                    .clickable {
                        haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                        onSend()
                    }
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
    statusText: String,
    reachability: ReachabilityLevel,
    avatarBytes: ByteArray?,
    onBack: () -> Unit,
    onOpenDetails: () -> Unit,
) {
    // T8: the contact's name + photo already live in Scaffold's topBar slot
    // (pinned above the message LazyColumn, never inside it), so they stay
    // visible while the conversation scrolls. A small persistent elevation
    // reinforces that visually, matching the tonalElevation/shadowElevation
    // this app already uses for content that floats above other surfaces.
    Surface(tonalElevation = 2.dp, shadowElevation = 2.dp) {
        TopAppBar(
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
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
                        reachability = reachability,
                        photoBytes = avatarBytes,
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
                            text = statusText,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            },
        )
    }
}

@Composable
private fun DaySeparator(timestampMs: Long) {
    val label = remember(timestampMs) {
        java.text.SimpleDateFormat("MMMM d, yyyy", java.util.Locale.getDefault()).format(java.util.Date(timestampMs))
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
        Text(text = stringResource(R.string.ui_some_messages_are_still_making_their_way_across),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f),
        )
    }
}

/**
 * Floating pill shown over the message list (FA7) when a new message arrived
 * while the reader was scrolled up; tapping it scrolls down to the newest
 * message. Shared between [ChatScreen] and [GroupChatScreen].
 */
@Composable
internal fun NewMessagesChip(onClick: () -> Unit, modifier: Modifier = Modifier) {
    Surface(
        onClick = onClick,
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.primary,
        contentColor = MaterialTheme.colorScheme.onPrimary,
        shadowElevation = 4.dp,
        modifier = modifier,
    ) {
        Text(
            text = stringResource(R.string.ui_new_messages),
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
        )
    }
}

/** Same corner-radius treatment 1:1 and group bubbles share: 6dp on the "joined" side, 20dp elsewhere. */
fun bubbleShapeFor(isOwn: Boolean, grouping: BubbleGrouping): RoundedCornerShape = RoundedCornerShape(
    topStart = if (!isOwn && grouping.joinsPrevious) 6.dp else 20.dp,
    topEnd = if (isOwn && grouping.joinsPrevious) 6.dp else 20.dp,
    bottomStart = if (!isOwn && grouping.joinsNext) 6.dp else 20.dp,
    bottomEnd = if (isOwn && grouping.joinsNext) 6.dp else 20.dp,
)

@Composable
private fun MessageBubble(
    message: StoredMessage,
    isOwn: Boolean,
    tick: TickStatus?,
    contactColor: Color?,
    grouping: BubbleGrouping,
    quoted: QuotedMessagePreview? = null,
    onQuotedClick: (StoredMessage) -> Unit = {},
    reactions: List<ReactionSummary> = emptyList(),
    onReact: (String) -> Unit = {},
    onPhotoClick: (ByteArray) -> Unit = {},
    outboundExpiryMs: Long? = null,
    onLongPress: (MessageTarget, Rect) -> Unit = { _, _ -> },
    onSwipeReply: () -> Unit = {},
) {
    var showLegend by remember { mutableStateOf(false) }
    var boundsInWindow by remember { mutableStateOf(Rect.Zero) }
    val topPadding = if (grouping.joinsPrevious) 2.dp else 10.dp
    val bottomPadding = if (grouping.joinsNext) 2.dp else 6.dp
    val shape = bubbleShapeFor(isOwn, grouping)
    val target = remember(message.senderUserId, message.lamport, message.kind) {
        MessageTarget(message.senderUserId, message.lamport, message.kind)
    }
    val photoBytes = remember(message.kind, message.payload) { messageImageBytes(message) }

    // Swipe-to-reply (T1): a rightward drag translates the bubble and reveals a
    // reply arrow; releasing past the threshold starts a reply and opens the
    // keyboard. Below threshold it just springs back. Vertical scrolling is
    // untouched -- detectHorizontalDragGestures only claims horizontal-dominant
    // drags, and a long-press (no movement) still opens the action overlay.
    val density = LocalDensity.current
    val thresholdPx = with(density) { 56.dp.toPx() }
    val maxDragPx = with(density) { 80.dp.toPx() }
    val offsetX = remember(target) { Animatable(0f) }
    val swipeScope = rememberCoroutineScope()
    val haptic = LocalHapticFeedback.current
    var passedThreshold by remember(target) { mutableStateOf(false) }
    val replyProgress = SwipeToReplyLogic.progress(offsetX.value, thresholdPx)

    Box(modifier = Modifier.fillMaxWidth()) {
        Icon(
            imageVector = ReplyIcon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.primary,
            modifier = Modifier
                .align(Alignment.CenterStart)
                .padding(start = 20.dp)
                .alpha(replyProgress)
                .scale(0.7f + 0.3f * replyProgress),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .offset { IntOffset(offsetX.value.roundToInt(), 0) }
                .pointerInput(target) {
                    detectHorizontalDragGestures(
                        onDragEnd = {
                            if (SwipeToReplyLogic.shouldReply(offsetX.value, thresholdPx)) {
                                onSwipeReply()
                            }
                            passedThreshold = false
                            swipeScope.launch { offsetX.animateTo(0f, spring()) }
                        },
                        onDragCancel = {
                            passedThreshold = false
                            swipeScope.launch { offsetX.animateTo(0f, spring()) }
                        },
                    ) { change, dragAmount ->
                        change.consume()
                        val next = SwipeToReplyLogic.clampOffset(offsetX.value + dragAmount, maxDragPx)
                        swipeScope.launch { offsetX.snapTo(next) }
                        if (!passedThreshold && next >= thresholdPx) {
                            passedThreshold = true
                            haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                        }
                    }
                }
                .padding(top = topPadding, bottom = bottomPadding),
            horizontalArrangement = if (isOwn) Arrangement.End else Arrangement.Start,
        ) {
            Column(horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start) {
            MessageBubbleVisual(
                message = message,
                isOwn = isOwn,
                tick = tick,
                contactColor = contactColor,
                shape = shape,
                reactions = reactions,
                onReact = onReact,
                quoted = quoted,
                onQuotedClick = quoted?.target?.let { target -> { onQuotedClick(target) } },
                modifier = Modifier
                    .onGloballyPositioned { coords -> boundsInWindow = coords.boundsInWindow() }
                    .messageActions(
                        onClick = {
                            if (photoBytes != null) {
                                onPhotoClick(photoBytes)
                            } else if (tick != null) {
                                showLegend = true
                            }
                        },
                        onLongClick = { onLongPress(target, boundsInWindow) },
                    ),
            )
            if (grouping.showTimestamp) {
                Text(
                    text = formatConversationTimestamp(message.timestamp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                )
            }
            if (isOwn && tick == TickStatus.SENT && outboundExpiryMs != null &&
                outboundExpiryMs <= System.currentTimeMillis()
            ) {
                Text(text = stringResource(R.string.ui_not_delivered),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 12.dp),
                )
            }
        }
        }
    }

    if (showLegend && tick != null) {
        LaunchedEffect(showLegend) {
            delay(2_500)
            showLegend = false
        }
        Surface(
            color = MaterialTheme.colorScheme.inverseSurface,
            contentColor = MaterialTheme.colorScheme.inverseOnSurface,
            shape = RoundedCornerShape(12.dp),
            modifier = Modifier.padding(horizontal = 16.dp),
        ) {
            Text(tickLegendText(tick), style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(10.dp))
        }
    }
}

/**
 * The bubble's visual only -- Surface with its content plus the reaction
 * chips below, no click handling. Used both by the list item ([MessageBubble])
 * and by [MessageFocusOverlay]'s undimmed floating copy, so the two render
 * pixel-identically.
 */
@Composable
fun MessageBubbleVisual(
    message: StoredMessage,
    isOwn: Boolean,
    tick: TickStatus?,
    contactColor: Color?,
    shape: RoundedCornerShape,
    reactions: List<ReactionSummary>,
    onReact: (String) -> Unit,
    modifier: Modifier = Modifier,
    quoted: QuotedMessagePreview? = null,
    onQuotedClick: (() -> Unit)? = null,
) {
    val bubbleColor = if (isOwn) {
        MaterialTheme.colorScheme.primary
    } else {
        contactColor?.copy(alpha = 0.24f) ?: MaterialTheme.colorScheme.surfaceVariant
    }
    val contentColor = if (isOwn) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface
    val tickBaseColor = if (bubbleColor.luminance() > 0.5f) Color.Black else Color.White

    Column(
        horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start,
        modifier = modifier.widthIn(max = 300.dp),
    ) {
        Surface(
            color = bubbleColor,
            contentColor = contentColor,
            shape = shape,
        ) {
            Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
                if (quoted != null) {
                    QuotedMessageBlock(
                        preview = quoted,
                        accentColor = if (isOwn) contentColor else MaterialTheme.colorScheme.primary,
                        contentColor = contentColor,
                        onClick = onQuotedClick,
                        modifier = Modifier.padding(bottom = 8.dp),
                    )
                }
                when (message.kind) {
                    KIND_ATTACHMENT_MANIFEST -> {
                        val attachment = remember(message.payload) {
                            AttachmentPayload.decode(message.payload)
                        }
                        if (attachment == null) {
                            Text(stringResource(R.string.ui_unsupported_attachment))
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

        if (reactions.isNotEmpty()) {
            ReactionRow(
                reactions = reactions,
                isOwn = isOwn,
                onReact = onReact,
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
fun Modifier.messageActions(
    onClick: () -> Unit = {},
    onLongClick: () -> Unit,
): Modifier = combinedClickable(
    onClick = onClick,
    onLongClick = onLongClick,
)

@Composable
fun ReactionRow(
    reactions: List<ReactionSummary>,
    isOwn: Boolean,
    onReact: (String) -> Unit,
) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.padding(
            start = if (isOwn) 0.dp else 10.dp,
            top = 3.dp,
            end = if (isOwn) 10.dp else 0.dp,
        ),
    ) {
        for (reaction in reactions) {
            Surface(
                shape = RoundedCornerShape(14.dp),
                color = if (reaction.reactedByOwnUser) {
                    MaterialTheme.colorScheme.primaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceVariant
                },
                // FA10: the pill itself stays ~24dp tall; minimumInteractiveComponentSize()
                // pads its touch target up to the 48dp minimum.
                modifier = Modifier
                    .minimumInteractiveComponentSize()
                    .clickable { onReact(reaction.emoji) },
            ) {
                val reactionLabel = if (reaction.count > 1) "${reaction.emoji} ${reaction.count}" else reaction.emoji
                Text(
                    text = reactionLabel,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                )
            }
        }
    }
}

private fun messageCopyText(message: StoredMessage): String =
    when (message.kind) {
        KIND_ATTACHMENT_MANIFEST -> AttachmentPayload.decode(message.payload)?.caption.orEmpty()
        else -> message.payload.toString(Charsets.UTF_8)
    }

internal fun messageImageBytes(message: StoredMessage): ByteArray? {
    if (message.kind != KIND_ATTACHMENT_MANIFEST) return null
    val attachment = AttachmentPayload.decode(message.payload) ?: return null
    return attachment.blob.takeIf { attachment.mediaType == AttachmentPayload.MediaType.IMAGE }
}

/**
 * One line of the Message-info sheet (fixes the colon-sniffing bug: the old
 * renderer built a single string and split each line on its first ":" to
 * fake "Label: value" styling, which misfired on the arrival line's own
 * "5:14 PM" timestamp -- there's no label there, but it has a colon too).
 * [messageInfoRows] returns each line pre-classified instead, so rendering
 * never has to guess.
 */
sealed class MessageInfoRow {
    data class LabelValue(val label: String, val value: String) : MessageInfoRow()
    data class Sentence(val text: String) : MessageInfoRow()
}

fun messageInfoRows(
    message: StoredMessage,
    isOwn: Boolean,
    tick: TickStatus?,
    arrival: MessageArrival? = null,
    deliveredViaRoute: String? = null,
    outboundExpiryMs: Long? = null,
    nowMs: Long = System.currentTimeMillis(),
): List<MessageInfoRow> {
    val sentAt = java.text.SimpleDateFormat(
        "MMMM d, yyyy h:mm a",
        java.util.Locale.getDefault(),
    ).format(java.util.Date(message.timestamp))
    val statusValue = when {
        isOwn && tick == TickStatus.SENT && outboundExpiryMs != null && outboundExpiryMs <= nowMs ->
            "Not delivered — expired"
        isOwn && tick == TickStatus.SENT && outboundExpiryMs != null ->
            "Still trying — expires in ${expiryRemainingText(outboundExpiryMs - nowMs)}"
        tick != null -> tickLegendText(tick)
        else -> null
    }
    // "Delivery confirmed via ..." and "Arrived via ..." are always plain
    // sentences -- neither is ever a genuine "Label: value" line, even
    // though the arrival sentence embeds a "h:mm a" time that can itself
    // contain a colon.
    val arrivalRow = when {
        isOwn -> deliveredViaRoute?.let { MessageInfoRow.Sentence("Delivery confirmed via $it") }
        else -> arrival?.let { MessageInfoRow.Sentence(messageArrivalText(it)) }
    }
    return listOfNotNull(
        MessageInfoRow.Sentence(if (isOwn) "Sent by you" else "Received"),
        MessageInfoRow.LabelValue("Time", sentAt),
        statusValue?.let { MessageInfoRow.LabelValue("Status", it) },
        arrivalRow,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MessageInfoBottomSheet(rows: List<MessageInfoRow>, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp).padding(bottom = 24.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.ui_message_info), style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_done)) }
            }
            for (row in rows) {
                when (row) {
                    is MessageInfoRow.LabelValue -> {
                        Text(row.label, style = MaterialTheme.typography.labelMedium, modifier = Modifier.padding(top = 12.dp))
                        Text(row.value, style = MaterialTheme.typography.bodyLarge)
                    }
                    is MessageInfoRow.Sentence -> {
                        Text(row.text, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.padding(top = 12.dp))
                    }
                }
            }
        }
    }
}

private fun expiryRemainingText(remainingMs: Long): String {
    val minutes = (remainingMs.coerceAtLeast(0L) + 59_999L) / 60_000L
    return when {
        minutes >= 2 * 24 * 60 -> "${(minutes + 1_439) / 1_440} days"
        minutes >= 24 * 60 -> "1 day"
        minutes >= 120 -> "${(minutes + 59) / 60} hours"
        minutes >= 60 -> "1 hour"
        else -> "$minutes minutes"
    }
}

internal fun transportRouteText(transport: Int): String =
    when (transport) {
        0 -> "direct BLE"
        1 -> "another device over BLE"
        2 -> "relay"
        3 -> "local Wi-Fi"
        4 -> "another device over local Wi-Fi"
        else -> "unknown route"
    }

private fun messageRouteText(arrival: MessageArrival): String =
    transportRouteText(arrival.transport.toInt())

private fun messageArrivalText(arrival: MessageArrival): String {
    val route = messageRouteText(arrival)
    // hopsTaken is inferred from the default hop TTL, so a sender that
    // authored with a non-default TTL skews it — present it as an estimate.
    val hops = arrival.hopsTaken.toInt()
    val hopLabel = "~$hops ${if (hops == 1) "hop" else "hops"}"
    val receivedAt = java.text.SimpleDateFormat(
        "h:mm a",
        java.util.Locale.getDefault(),
    ).format(java.util.Date(arrival.receivedAt))
    return "Arrived via $route · $hopLabel · $receivedAt"
}

@Composable
internal fun AttachmentBubbleContent(
    attachment: AttachmentPayload,
    contentColor: Color,
) {
    when (attachment.mediaType) {
        AttachmentPayload.MediaType.IMAGE -> {
            ChatImageAttachment(jpeg = attachment.blob)
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

/**
 * Renders a chat photo at its native aspect ratio (no center-crop), capped to
 * the bubble width and a reasonable max height. Gesture handling stays on the
 * outer bubble so tap can open the full-screen viewer and long-press can open
 * the existing message-focus overlay without nested click targets competing.
 */
@Composable
private fun ChatImageAttachment(jpeg: ByteArray) {
    // Header-only decode (no pixel buffer) so layout size is known
    // immediately; the actual pixels are decoded downsampled, off the main
    // thread, below (FA4).
    val bounds = remember(jpeg) { ChatImageDecoder.decodeBounds(jpeg) }

    if (bounds == null) {
        Text(stringResource(R.string.ui_photo_could_not_display))
        return
    }
    val (sourceWidth, sourceHeight) = bounds

    BoxWithConstraints(modifier = Modifier.fillMaxWidth()) {
        val density = LocalDensity.current
        val maxWidthPx = with(density) { maxWidth.toPx() }
        val maxHeightPx = with(density) { 360.dp.toPx() }
        val (widthPx, heightPx) = remember(sourceWidth, sourceHeight, maxWidthPx, maxHeightPx) {
            ImageGallery.fitSize(sourceWidth, sourceHeight, maxWidthPx, maxHeightPx)
        }
        val widthDp = with(density) { widthPx.toDp() }
        val heightDp = with(density) { heightPx.toDp() }

        val imageBitmap by produceState<ImageBitmap?>(null, jpeg, widthPx, heightPx) {
            value = withContext(Dispatchers.IO) {
                ChatImageDecoder.decodeSampled(jpeg, widthPx.roundToInt(), heightPx.roundToInt())
                    ?.asImageBitmap()
            }
        }
        val currentBitmap = imageBitmap

        if (currentBitmap == null) {
            // Reserve the final layout size while the downsampled decode runs
            // in the background, so nothing jumps once it lands.
            Box(
                modifier = Modifier
                    .size(widthDp, heightDp)
                    .clip(RoundedCornerShape(12.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant),
            )
        } else {
            Image(
                bitmap = currentBitmap,
                contentDescription = "Photo — tap to view full screen",
                contentScale = ContentScale.Fit,
                modifier = Modifier
                    .size(widthDp, heightDp)
                    .clip(RoundedCornerShape(12.dp)),
            )
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
    val scope = rememberCoroutineScope()
    var playing by remember { mutableStateOf(false) }
    // Guards against a double-tap starting a second load (and a second temp
    // file / MediaPlayer) while the first one is still preparing.
    var loading by remember { mutableStateOf(false) }
    var player by remember { mutableStateOf<MediaPlayer?>(null) }
    // FA11: the play-<ts>.m4a temp file this player is backed by, if any --
    // tracked so every exit path (manual stop, dispose, completion) can
    // delete it instead of only the completion listener doing so.
    var tempFile by remember { mutableStateOf<File?>(null) }

    fun stopAndCleanup() {
        player?.let { mp ->
            try {
                mp.stop()
            } catch (_: IllegalStateException) {
                // Already stopped/released elsewhere -- nothing more to do to the player.
            }
            mp.release()
        }
        player = null
        playing = false
        tempFile?.delete()
        tempFile = null
    }

    DisposableEffect(blob) {
        onDispose { stopAndCleanup() }
    }

    Row(verticalAlignment = Alignment.CenterVertically) {
        IconButton(
            onClick = {
                when {
                    playing -> stopAndCleanup()
                    loading -> Unit
                    else -> {
                        loading = true
                        scope.launch {
                            // FA11: writing the blob to disk and MediaPlayer.prepare()
                            // (a blocking decode of the audio headers) both used to
                            // run synchronously on the main thread in this click handler.
                            val prepared = withContext(Dispatchers.IO) {
                                try {
                                    val temp = File(context.cacheDir, "play-${System.currentTimeMillis()}.m4a")
                                    temp.writeBytes(blob)
                                    val mp = MediaPlayer()
                                    mp.setDataSource(temp.absolutePath)
                                    mp.prepare()
                                    temp to mp
                                } catch (_: Exception) {
                                    null
                                }
                            }
                            loading = false
                            if (prepared == null) {
                                Toast.makeText(context, "Could not play voice memo", Toast.LENGTH_SHORT).show()
                                return@launch
                            }
                            val (temp, mp) = prepared
                            if (!isActive) {
                                // The screen was left mid-load -- don't leak the player or its temp file.
                                mp.release()
                                temp.delete()
                                return@launch
                            }
                            mp.setOnCompletionListener {
                                playing = false
                                it.release()
                                player = null
                                temp.delete()
                                tempFile = null
                            }
                            tempFile = temp
                            player = mp
                            playing = true
                            mp.start()
                        }
                    }
                }
            },
            // FA10: keep the 40dp visual size, but restore a 48dp touch target
            // (a caller-supplied .size() below IconButton's own would otherwise
            // shrink its built-in minimum back down).
            modifier = Modifier.minimumInteractiveComponentSize().size(40.dp),
        ) {
            Icon(
                imageVector = if (playing) Icons.Default.Close else Icons.Default.PlayArrow,
                contentDescription = if (playing) "Stop" else "Play voice memo",
                tint = contentColor,
            )
        }
        Text(
            text = stringResource(R.string.ui_voice_memo_duration, formatDurationMs(durationMs)),
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
            snackbarHostState = remember { SnackbarHostState() },
            draft = "",
            onDraftChange = {},
            onSend = {},
            onBack = {},
            onDeleteContact = {},
        )
    }
}
