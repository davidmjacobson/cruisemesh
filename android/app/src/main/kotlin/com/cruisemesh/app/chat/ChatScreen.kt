package com.cruisemesh.app.chat

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.content.Context
import android.net.Uri
import android.os.SystemClock
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
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
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
import androidx.compose.foundation.layout.height
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
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
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
import androidx.compose.runtime.rememberUpdatedState
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
import androidx.compose.ui.input.pointer.AwaitPointerEventScope
import androidx.compose.ui.input.pointer.PointerId
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.core.content.ContextCompat
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.ChatImageDecoder
import com.cruisemesh.app.media.ImageGallery
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import com.cruisemesh.app.media.LocalVoiceMessagePlayback
import com.cruisemesh.app.media.MediaCompressor
import com.cruisemesh.app.media.VoiceRecorder
import com.cruisemesh.app.media.createCameraCaptureUri
import com.cruisemesh.app.media.isVisibleChatKind
import com.cruisemesh.app.media.rememberVoiceMessagePlayback
import com.cruisemesh.app.mesh.ContactReachability
import com.cruisemesh.app.notify.ChatMuteStore
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.ui.AvatarBadge
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.friending.ShareContactAvailability
import com.cruisemesh.app.friending.ShareContactPolicy
import com.cruisemesh.app.ui.ABUSE_REPORT_ADDRESS
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ComposerCameraIcon
import com.cruisemesh.app.ui.ComposerMicIcon
import com.cruisemesh.app.ui.ComposerPauseIcon
import com.cruisemesh.app.ui.ComposerSendIcon
import com.cruisemesh.app.ui.ContactReportOutcome
import com.cruisemesh.app.ui.ContactDetailsSheet
import com.cruisemesh.app.ui.ConversationMessageMeta
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.ReplyIcon
import com.cruisemesh.app.ui.SignalTick
import com.cruisemesh.app.ui.bubbleGroupingFor
import com.cruisemesh.app.ui.copyAbuseReportAddress
import com.cruisemesh.app.ui.formatConversationTimestamp
import com.cruisemesh.app.ui.launchContactReport
import com.cruisemesh.app.ui.tickLegendText
import uniffi.cruisemesh_core.ComposerReach
import uniffi.cruisemesh_core.CoreVoiceCaptureState
import uniffi.cruisemesh_core.ConsumedHiddenLamport
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.MessageArrival
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.VoiceCaptureEffect
import uniffi.cruisemesh_core.VoiceCapturePhase
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.voiceCaptureBytes
import uniffi.cruisemesh_core.voiceCaptureCancel
import uniffi.cruisemesh_core.voiceCaptureDrag
import uniffi.cruisemesh_core.voiceCaptureElapsed
import uniffi.cruisemesh_core.voiceCaptureFinish
import uniffi.cruisemesh_core.voiceCapturePress
import uniffi.cruisemesh_core.voiceCaptureRelease
import uniffi.cruisemesh_core.voiceCaptureStartHandsFree
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlin.math.roundToInt
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

/** `receipt_type` values (DESIGN.md §7.2), for reading own-message tick watermarks out of the store. */
private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u

// The duration bound and the accidental-tap threshold used to live here as
// literals, and iOS carried its own copies. Both now come from the core's
// `voiceCapturePlan()`, which derives the bound from the attachment blob cap.

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
    /** Their friend card's relay endpoint has been written off after rejecting us (core `contact_relay_health`). */
    relayCardIsStale: Boolean = false,
    /**
     * Which direction of this chat cannot cross the internet, from core
     * `composer_reach`. Purely local; drives the notice above the composer.
     */
    composerReach: ComposerReach = ComposerReach.FINE,
    /** Opens the share-contact code for this contact (specs/share-contact.md). */
    onShareContact: (Contact) -> Unit = {},
) {
    val context = LocalContext.current
    var currentContact by remember(contact.userId) { mutableStateOf(contact) }
    var messages by remember(contact.userId) { mutableStateOf(store.messagesForChat(currentContact.userId)) }
    var consumedHiddenLamports by remember(contact.userId) {
        mutableStateOf(store.consumedHiddenLamports(currentContact.userId))
    }
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
    var isBlocked by remember(contact.userId) { mutableStateOf(store.isUserBlocked(contact.userId)) }
    val shareAvailability = remember(contact.userId, isBlocked) {
        ShareContactPolicy.availability(store.getContactDiscoveryPolicy(contact.userId), isBlocked)
    }
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
        consumedHiddenLamports = store.consumedHiddenLamports(currentContact.userId)
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
                mimeType = VoiceRecorder.plan.mimeType,
                durationMs = durationMs,
                blob = bytes,
            ),
            replyingTo?.let(::replyTargetId),
        )
        if (result == SendResult.STORED) {
            replyingTo = null
            reload()
        } else {
            // The recording file is already gone, so the generic "still here"
            // copy would be wrong for a voice message.
            showSendFailure(context.getString(R.string.ui_could_not_send_voice_message))
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
            Toast.makeText(context, context.getString(R.string.ui_microphone_ready), Toast.LENGTH_SHORT).show()
        } else {
            Toast.makeText(
                context,
                context.getString(R.string.ui_microphone_permission_needed),
                Toast.LENGTH_SHORT,
            ).show()
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
        consumedHiddenLamports = consumedHiddenLamports,
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
            // stop() drains the recorder's buffered tail off the UI thread, then
            // delivers the finalized file (or null) back on the main thread.
            voiceRecorder.stop { result ->
                if (result != null) {
                    sendVoiceFile(result.first, result.second)
                } else {
                    Toast.makeText(
                        context,
                        context.getString(R.string.ui_voice_recording_failed),
                        Toast.LENGTH_SHORT,
                    ).show()
                }
            }
        },
        onCancelVoice = { voiceRecorder.cancel() },
        onVoiceBytesRecorded = { voiceRecorder.bytesRecorded() },
        onBack = onBack,
        onDeleteContact = onDeleteContact,
        reachability = reachability,
        reachabilityStatusText = reachabilityStatusText,
        reachabilityDetailsText = reachabilityDetailsText,
        relayCardIsStale = relayCardIsStale,
        composerReach = composerReach,
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
        isBlocked = isBlocked,
        onBlockedChange = { blocked ->
            if (blocked) {
                store.blockUser(currentContact.userId, System.currentTimeMillis())
            } else {
                store.unblockUser(currentContact.userId)
            }
            isBlocked = blocked
        },
        onReport = {
            if (launchContactReport(context, currentContact, ownUserId) == ContactReportOutcome.ADDRESS_COPIED) {
                coroutineScope.launch {
                    val result = snackbarHostState.showSnackbar(
                        message = context.getString(R.string.ui_no_email_app, ABUSE_REPORT_ADDRESS),
                        actionLabel = context.getString(R.string.ui_copy),
                        withDismissAction = true,
                        duration = SnackbarDuration.Indefinite,
                    )
                    if (result == SnackbarResult.ActionPerformed) {
                        copyAbuseReportAddress(context)
                    }
                }
            }
        },
        shareAvailability = shareAvailability,
        onShareContact = { onShareContact(currentContact) },
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
    consumedHiddenLamports: List<ConsumedHiddenLamport> = emptyList(),
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
    onVoiceBytesRecorded: () -> Long = { 0L },
    onReact: (MessageTarget, String) -> Unit = { _, _ -> },
    onBack: () -> Unit,
    onDeleteContact: () -> Unit,
    reachability: ReachabilityLevel = ReachabilityLevel.OFFLINE,
    reachabilityStatusText: String = ContactReachability.chatHeaderCopy(ReachabilityLevel.OFFLINE, null, 0L),
    reachabilityDetailsText: String = reachabilityStatusText,
    /** Their friend card's relay endpoint has been written off after rejecting us (core `contact_relay_health`). */
    relayCardIsStale: Boolean = false,
    composerReach: ComposerReach = ComposerReach.FINE,
    isMuted: Boolean = false,
    onMutedChange: (Boolean) -> Unit = {},
    onSetNickname: (String?) -> Unit = {},
    isBlocked: Boolean = false,
    onBlockedChange: (Boolean) -> Unit = {},
    onReport: () -> Unit = {},
    shareAvailability: ShareContactAvailability = ShareContactAvailability.HIDDEN,
    onShareContact: () -> Unit = {},
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
    val linkHandler = rememberMessageLinkHandler()
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
    val gaps = remember(messages, consumedHiddenLamports) {
        visibleGapIndices(messages, consumedHiddenLamports)
    }
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

    ConversationHostEffects(host, visibleMessages, ownUserId, chatExtras.lateArrivalMs.keys)

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

                // Keep the separator and its first message in one lazy-list
                // item. With reverseLayout, emitting multiple roots from the
                // item lambda placed those roots bottom-to-top, so the first
                // message of a day appeared above its date separator.
                Column {
                    if (isNewDay(visibleMessages, index)) {
                        DaySeparator(message.timestamp)
                    }

                    if (gaps.contains(index)) {
                        GapIndicator()
                    }

                    MessageBubble(
                        message = message,
                        isFocused = host.focused?.target == MessageTarget(
                            message.senderUserId,
                            message.lamport,
                            message.kind,
                        ),
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
                        lateArrivalMs = chatExtras.lateArrivalMs[messageStableKey(message)],
                        onLongPress = { target, bounds -> openOverlay(target, bounds) },
                        onSwipeReply = { startReply(message) },
                        onLinkClick = { link -> linkHandler.open(link) },
                    )
                }
            }
        },
        belowList = {
            ComposerReachNotice(reach = composerReach, contactName = displayName)

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
                bytesRecorded = onVoiceBytesRecorded,
            )
        },
        overlays = {
            MessageLinkPrompt(linkHandler)

            if (showContactDetails) {
                ContactDetailsSheet(
                    contact = contact,
                    connectivityText = reachabilityDetailsText,
                    isMuted = isMuted,
                    onMutedChange = onMutedChange,
                    onSetNickname = onSetNickname,
                    isBlocked = isBlocked,
                    onBlockedChange = onBlockedChange,
                    onReport = {
                        showContactDetails = false
                        onReport()
                    },
                    relayCardIsStale = relayCardIsStale,
                    shareAvailability = shareAvailability,
                    onShareContact = {
                        showContactDetails = false
                        onShareContact()
                    },
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
                    val focusedImage = remember(focusedMessage.payload, focusedMessage.kind) { messageImageBytes(focusedMessage) }
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
                        onSaveImage = focusedImage?.let { jpeg ->
                            {
                                val saved = ImageGallery.saveJpeg(context, jpeg)
                                Toast.makeText(
                                    context,
                                    if (saved != null) "Saved to Pictures/CruiseMesh" else "Could not save image",
                                    Toast.LENGTH_SHORT,
                                ).show()
                                closeOverlay()
                            }
                        },
                        onInfo = {
                            host.openInfo(focusedMessage)
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
            onDismiss = { host.closeInfo() },
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
 * The one place a person is guaranteed to look before typing: a persistent,
 * non-modal line above the composer saying which direction of this chat cannot
 * cross the internet. Renders nothing for [ComposerReach.FINE], which is every
 * ordinary chat.
 *
 * Deliberately not a dialog, a snackbar, or a row inside the contact sheet
 * three taps away. The failure it describes is silent -- messages sit at one
 * tick forever and no screen explains why -- so it has to be where the typing
 * happens, and it has to stay put.
 */
@Composable
internal fun ComposerReachNotice(reach: ComposerReach, contactName: String, modifier: Modifier = Modifier) {
    val stringRes = ComposerReachCopy.stringResFor(reach) ?: return
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
    ) {
        Text(
            text = stringResource(stringRes, contactName),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
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
    var bitmap by remember(bytes, previewPx) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(bytes, previewPx) {
        bitmap = withContext(Dispatchers.IO) {
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
 * icon inside on the right, and a trailing action that is a hold-to-talk
 * microphone when the draft is empty and a send button once there's text.
 *
 * Push-to-talk records while the mic is held and sends on release. Sliding
 * left cancels; sliding up locks the recording hands-free so the finger can
 * come off. Every threshold, the minimum hold that counts as speech, the
 * duration bound and the byte budget belong to the core ([VoiceRecorder.plan],
 * [voiceCapturePress] and friends), so this screen and the iOS composer cannot
 * disagree about what the gesture means. The recorder itself is owned by
 * [ChatScreen]; this composable only drives it through [onStartVoice] /
 * [onStopVoice] / [onCancelVoice].
 *
 * A hold is a gesture some people cannot make, so the mic also carries a plain
 * "start voice message" accessibility action that goes straight to the
 * hands-free state, where Cancel and Send are ordinary buttons.
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
    /**
     * Bytes the encoder has written so far. Weighed on every tick because the
     * duration bound only holds if the encoder honoured the bitrate it was
     * asked for, and some do not; see `core/src/voice.rs`.
     */
    bytesRecorded: () -> Long = { 0L },
    // Monotonic, not a frame count and not wall time: a stalled frame must not
    // let a recording run past the byte budget, and a clock correction mid-hold
    // (time-zone change at sea, cell reacquisition in port) must not shorten or
    // lengthen it either. Injectable only so the gesture tests can hold a press
    // for a plausible number of seconds without sleeping for them.
    nowMs: () -> Long = SystemClock::elapsedRealtime,
) {
    val context = LocalContext.current
    val haptic = LocalHapticFeedback.current
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    val onBubbleColor = MaterialTheme.colorScheme.onPrimary
    val capture = remember { mutableStateOf(IDLE_VOICE_CAPTURE) }
    val recordingStartedAt = remember { mutableLongStateOf(0L) }
    val now by rememberUpdatedState(nowMs)
    val recordedBytes by rememberUpdatedState(bytesRecorded)
    val startVoice by rememberUpdatedState(onStartVoice)
    val stopVoice by rememberUpdatedState(onStopVoice)
    val cancelVoice by rememberUpdatedState(onCancelVoice)
    val tooShortMessage = stringResource(R.string.ui_hold_the_mic_to_talk)
    val leftTheAppMessage = stringResource(R.string.ui_recording_stopped_left_app)
    val holdToTalkLabel = stringResource(R.string.ui_hold_to_talk)
    val startVoiceLabel = stringResource(R.string.ui_start_voice_message)
    val sendVoiceLabel = stringResource(R.string.ui_send_voice_message)
    val recording = capture.value.phase != VoiceCapturePhase.IDLE
    // A staged photo can be sent on its own, so the send button shows whenever
    // there's text *or* a pending attachment; the mic only takes over when the
    // composer is otherwise empty.
    val canSend = draft.isNotBlank() || hasPendingAttachment

    fun elapsedMs(): UInt =
        (now() - recordingStartedAt.longValue)
            .coerceIn(0L, UInt.MAX_VALUE.toLong())
            .toUInt()

    fun applyEffect(effect: VoiceCaptureEffect) {
        when (effect) {
            VoiceCaptureEffect.SEND -> stopVoice()
            VoiceCaptureEffect.DISCARD_TOO_SHORT -> {
                cancelVoice()
                Toast.makeText(context, tooShortMessage, Toast.LENGTH_SHORT).show()
            }
            VoiceCaptureEffect.DISCARD_CANCELLED -> cancelVoice()
            VoiceCaptureEffect.START, VoiceCaptureEffect.NONE -> Unit
        }
    }

    LaunchedEffect(recording) {
        if (!recording) return@LaunchedEffect
        while (capture.value.phase != VoiceCapturePhase.IDLE) {
            val ticked = voiceCaptureElapsed(capture.value, elapsedMs())
            capture.value = ticked.state
            applyEffect(ticked.effect)
            if (capture.value.phase != VoiceCapturePhase.IDLE) {
                // The clock is not the only bound: an encoder that ignores the
                // bitrate we asked for fills the envelope early, and finding
                // that out after the user has spoken is the failure this
                // package exists to remove.
                val written = recordedBytes().coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt()
                val weighed = voiceCaptureBytes(capture.value, written)
                capture.value = weighed.state
                applyEffect(weighed.effect)
            }
            delay(100)
        }
    }

    DisposableEffect(lifecycleOwner) {
        // A hands-free recording outlives the finger, so it can outlive the
        // screen too: pressing Home, taking a call, or pulling down a
        // notification all leave the app in the background, where Android
        // feeds a recorder without a microphone foreground-service type
        // silence. Sending a minute of that is worse than losing the
        // recording, so stop and say so.
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == androidx.lifecycle.Lifecycle.Event.ON_STOP &&
                capture.value.phase != VoiceCapturePhase.IDLE
            ) {
                capture.value = voiceCaptureCancel(capture.value).state
                cancelVoice()
                Toast.makeText(context, leftTheAppMessage, Toast.LENGTH_SHORT).show()
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            // Leaving the screen mid-recording must not leave the mic hot.
            if (capture.value.phase != VoiceCapturePhase.IDLE) {
                capture.value = IDLE_VOICE_CAPTURE
                cancelVoice()
            }
        }
    }

    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(com.cruisemesh.app.ui.UiTestTags.MESSAGE_COMPOSER)
            .padding(bottom = 16.dp),
    ) {
        Box(
            modifier = Modifier
                // Keep the semantic hit target at Android's 48dp minimum. A
                // size modifier after minimumInteractiveComponentSize would
                // constrain the node back down to the visual size.
                .size(48.dp)
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
            RecordingPill(
                state = capture.value,
                modifier = Modifier.weight(1f),
                onCancel = {
                    val step = voiceCaptureCancel(capture.value)
                    capture.value = step.state
                    applyEffect(step.effect)
                },
            )
        } else {
            TextField(
                value = draft,
                onValueChange = onDraftChange,
                placeholder = {
                    Text(stringResource(if (hasPendingAttachment) R.string.ui_add_a_caption else R.string.ui_message))
                },
                trailingIcon = {
                    IconButton(
                        onClick = onPickCamera,
                        modifier = Modifier.size(48.dp),
                    ) {
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
        } else if (capture.value.phase == VoiceCapturePhase.LOCKED) {
            // Hands-free: the finger is off the button, so the same slot becomes
            // an ordinary send control.
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .background(ownBubbleColor)
                    .clickable {
                        val step = voiceCaptureFinish(capture.value, elapsedMs())
                        capture.value = step.state
                        applyEffect(step.effect)
                    }
                    .semantics { contentDescription = sendVoiceLabel },
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
                        awaitEachGesture {
                            val down = awaitFirstDown(requireUnconsumed = false)
                            val pressed = voiceCapturePress(capture.value)
                            if (pressed.effect != VoiceCaptureEffect.START || !startVoice()) {
                                // Either a recording is already running or the
                                // mic was refused: swallow the rest of the
                                // gesture so nothing half-starts.
                                waitForRelease(down.id)
                                return@awaitEachGesture
                            }
                            recordingStartedAt.longValue = now()
                            capture.value = pressed.state
                            haptic.performHapticFeedback(HapticFeedbackType.LongPress)

                            while (true) {
                                val event = awaitPointerEvent()
                                val change = event.changes.firstOrNull { it.id == down.id } ?: break
                                if (!change.pressed) break
                                if (capture.value.phase != VoiceCapturePhase.HOLDING) break
                                val dx = (change.position.x - down.position.x).toDp().value
                                val dy = (change.position.y - down.position.y).toDp().value
                                capture.value = voiceCaptureDrag(capture.value, dx, dy).state
                            }

                            val step = voiceCaptureRelease(capture.value, elapsedMs())
                            capture.value = step.state
                            applyEffect(step.effect)
                        }
                    }
                    .semantics {
                        contentDescription = holdToTalkLabel
                        role = Role.Button
                        // Hold-to-talk is unreachable through a screen reader or
                        // a switch: neither can express "press and keep
                        // pressing". This action starts the same recording
                        // hands-free, where Cancel and Send are plain buttons.
                        onClick(label = startVoiceLabel) {
                            val started = voiceCaptureStartHandsFree(capture.value)
                            if (started.effect != VoiceCaptureEffect.START || !startVoice()) {
                                return@onClick false
                            }
                            recordingStartedAt.longValue = now()
                            capture.value = started.state
                            true
                        }
                    },
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

/**
 * The core's idle capture state, restated as plain data.
 *
 * Everything the gesture *means* still comes from the core; this exists only
 * because the composer must render before any gesture happens, and the Compose
 * preview screenshot renderer inflates it in a sandbox that cannot load the
 * native library. `MessageComposerCoreContractTest` asserts this stays equal to
 * `voiceCaptureIdleState()`.
 *
 * A `get()` rather than a stored value: the generated record's fields are `var`,
 * and a shared instance handed to several composers would be one stray write
 * away from a very confusing bug.
 */
internal val IDLE_VOICE_CAPTURE: CoreVoiceCaptureState
    get() = CoreVoiceCaptureState(
        phase = VoiceCapturePhase.IDLE,
        cancelArmed = false,
        lockArmed = false,
        elapsedMs = 0u,
    )

/** Drains the rest of a pointer gesture this composer is not acting on. */
private suspend fun AwaitPointerEventScope.waitForRelease(pointerId: PointerId) {
    while (true) {
        val event = awaitPointerEvent()
        val change = event.changes.firstOrNull { it.id == pointerId } ?: return
        if (!change.pressed) return
    }
}

/**
 * Replaces the text field while a voice message is being recorded: a live red
 * pip, the elapsed time, and what the gesture would do if the finger came off
 * right now.
 */
@Composable
private fun RecordingPill(
    state: CoreVoiceCaptureState,
    modifier: Modifier = Modifier,
    onCancel: () -> Unit,
) {
    val locked = state.phase == VoiceCapturePhase.LOCKED
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .clip(RoundedCornerShape(24.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(horizontal = 16.dp, vertical = 14.dp),
    ) {
        Box(
            modifier = Modifier
                .size(10.dp)
                .clip(CircleShape)
                .background(if (state.cancelArmed) Color(0xFF9E9E9E) else Color(0xFFE53935)),
        )
        Spacer(modifier = Modifier.width(10.dp))
        Text(stringResource(R.string.ui_recording_duration, formatDurationMs(state.elapsedMs.toInt())))
        Spacer(modifier = Modifier.width(10.dp))
        if (locked) {
            Spacer(modifier = Modifier.weight(1f))
            TextButton(onClick = onCancel) {
                Text(stringResource(R.string.ui_cancel_recording))
            }
        } else {
            Text(
                text = when {
                    state.cancelArmed -> stringResource(R.string.ui_release_to_cancel)
                    state.lockArmed -> stringResource(R.string.ui_release_for_hands_free)
                    else -> stringResource(R.string.ui_slide_to_cancel_or_lock)
                },
                style = MaterialTheme.typography.labelSmall,
                // The hint takes the leftover width rather than whatever is
                // left after it: unweighted, at a large font scale it wraps to
                // four or five lines and grows the pill up over the message
                // list at exactly the moment the user is reading it.
                modifier = Modifier.weight(1f),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.End,
                color = if (state.cancelArmed) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
            )
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
    isFocused: Boolean,
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
    /** When this message reached this device, if its place in the thread needs explaining. */
    lateArrivalMs: Long? = null,
    onLongPress: (MessageTarget, Rect) -> Unit = { _, _ -> },
    onSwipeReply: () -> Unit = {},
    onLinkClick: (MessageLink) -> Unit = {},
) {
    var showLegend by remember { mutableStateOf(false) }
    var boundsInRoot by remember { mutableStateOf(Rect.Zero) }
    val topPadding = if (grouping.joinsPrevious) 2.dp else 10.dp
    val bottomPadding = if (grouping.joinsNext) 2.dp else 6.dp
    val shape = bubbleShapeFor(isOwn, grouping)
    val target = remember(message.senderUserId, message.lamport, message.kind) {
        MessageTarget(message.senderUserId, message.lamport, message.kind)
    }
    val photoBytes = remember(message.kind, message.payload) { messageImageBytes(message) }
    val onBubbleClick = {
        if (photoBytes != null) {
            onPhotoClick(photoBytes)
        } else if (tick != null) {
            showLegend = true
        }
    }

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
                // The body text takes both gestures back over its own glyphs
                // (a link needs the tap position) and re-emits them, so a tap
                // off a link and a long-press anywhere behave the same as they
                // do on the rest of the bubble.
                bodyActions = MessageBodyActions(
                    onLinkClick = onLinkClick,
                    onClick = onBubbleClick,
                    // No haptic here on purpose: combinedClickable's long-click
                    // doesn't buzz either (Compose 1.7), and a link that felt
                    // different from the rest of the bubble would read as a
                    // different gesture.
                    onLongClick = { onLongPress(target, boundsInRoot) },
                ),
                modifier = Modifier
                    // The overlay redraws this visual at its source position
                    // before moving it. Hide the list copy so the motion does
                    // not leave a dim "ghost" bubble behind.
                    .alpha(if (isFocused) 0f else 1f)
                    .onGloballyPositioned { coords -> boundsInRoot = coords.unclippedBoundsInRoot() }
                    .messageActions(
                        onClick = onBubbleClick,
                        onLongClick = { onLongPress(target, boundsInRoot) },
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
            // Only set when this message was spliced in above content that was
            // already here (core/src/late_arrival.rs), which is the one case
            // where its position needs explaining. The bubble keeps the
            // sender's time; this says when it reached us.
            if (lateArrivalMs != null) {
                Text(
                    text = stringResource(R.string.ui_arrived_at, formatConversationTimestamp(lateArrivalMs)),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 12.dp),
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
    bodyActions: MessageBodyActions? = null,
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
                            AttachmentBubbleContent(
                                attachment = attachment,
                                messageKey = messageItemKey(message),
                                contentColor = contentColor,
                                isOwn = isOwn,
                                bodyActions = bodyActions,
                            )
                        }
                    }
                    else -> {
                        MessageBodyText(
                            body = message.payload.toString(Charsets.UTF_8),
                            isOwn = isOwn,
                            actions = bodyActions,
                        )
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

/**
 * How far the reaction row is pulled back up under the bubble.
 *
 * A chip is a ~24dp pill, but [minimumInteractiveComponentSize] grows its
 * touch target to 48dp, which hangs ~12dp of invisible padding above the
 * pill. Laid out naively that padding reads as a gap, so the chips floated
 * ~15dp below the bubble instead of tucking under its bottom edge. This
 * cancels the top half of that padding; the pill itself is untouched and so
 * is the 48dp target.
 */
private val REACTION_ROW_TUCK = 12.dp

/**
 * Pull the row up by [amount] *and* shrink the space it claims by the same
 * amount, so tucking the chips under the bubble doesn't leave a dead band
 * before the next message. The bottom edge of the content stays flush with
 * the bottom of the reported bounds, so the whole touch target is still
 * inside them; only transparent padding ends up above the row, overlapping
 * the bubble's own (in-bounds, and therefore tap-winning) area.
 */
private fun Modifier.tuckUnderBubble(amount: Dp): Modifier = layout { measurable, constraints ->
    val placeable = measurable.measure(constraints)
    val tuck = amount.roundToPx().coerceAtMost(placeable.height)
    layout(placeable.width, placeable.height - tuck) {
        placeable.place(0, -tuck)
    }
}

@Composable
fun ReactionRow(
    reactions: List<ReactionSummary>,
    isOwn: Boolean,
    onReact: (String) -> Unit,
) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier
            .tuckUnderBubble(REACTION_ROW_TUCK)
            .padding(
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

internal fun messageCopyText(message: StoredMessage): String =
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
        0 -> "direct Bluetooth"
        1 -> "another device over Bluetooth"
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
    /**
     * Identifies the message across reloads ([messageItemKey]), so voice
     * playback follows the message rather than the payload array the store
     * happened to hand back this time.
     */
    messageKey: String,
    contentColor: Color,
    // A caption is a message body -- iOS linkifies it, so this does too, or a
    // link is tappable on one platform and dead on the other (6.6).
    isOwn: Boolean = false,
    bodyActions: MessageBodyActions? = null,
) {
    when (attachment.mediaType) {
        AttachmentPayload.MediaType.IMAGE -> {
            ChatImageAttachment(jpeg = attachment.blob)
            if (attachment.caption.isNotBlank()) {
                MessageBodyText(
                    body = attachment.caption,
                    isOwn = isOwn,
                    modifier = Modifier.padding(top = 6.dp),
                    actions = bodyActions,
                )
            }
        }
        AttachmentPayload.MediaType.AUDIO -> {
            VoiceMemoPlayer(
                messageKey = messageKey,
                blob = attachment.blob,
                durationMs = attachment.durationMs,
                contentColor = contentColor,
            )
            if (attachment.caption.isNotBlank()) {
                MessageBodyText(
                    body = attachment.caption,
                    isOwn = isOwn,
                    modifier = Modifier.padding(top = 6.dp),
                    actions = bodyActions,
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

        var imageBitmap by remember(jpeg, widthPx, heightPx) { mutableStateOf<ImageBitmap?>(null) }
        LaunchedEffect(jpeg, widthPx, heightPx) {
            imageBitmap = withContext(Dispatchers.IO) {
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

/**
 * Inline voice-message bubble: play/pause, a progress bar, and elapsed over
 * total.
 *
 * Draws whatever the conversation's
 * [com.cruisemesh.app.media.VoiceMessagePlayback] says about this
 * message and hands taps back to it; it owns no player of its own, so a chat
 * reload or a scroll cannot stop a message that is playing. See that class for
 * the bug that put it there.
 */
@Composable
private fun VoiceMemoPlayer(
    messageKey: String,
    blob: ByteArray,
    durationMs: Int,
    contentColor: Color,
) {
    // Outside a conversation -- a preview, a bubble on its own in a test --
    // there is nothing to stay continuous with, so the bubble plays its own.
    val playback = LocalVoiceMessagePlayback.current ?: rememberVoiceMessagePlayback()
    val state = playback.stateFor(messageKey, durationMs)

    Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(
                onClick = { playback.toggle(messageKey, blob, durationMs) },
                // FA10: keep the 40dp visual size, but restore a 48dp touch target
                // (a caller-supplied .size() below IconButton's own would otherwise
                // shrink its built-in minimum back down).
                modifier = Modifier.minimumInteractiveComponentSize().size(40.dp),
            ) {
                Icon(
                    imageVector = if (state.isPlaying) ComposerPauseIcon else Icons.Default.PlayArrow,
                    contentDescription = stringResource(
                        if (state.isPlaying) R.string.ui_pause_voice_message else R.string.ui_play_voice_message,
                    ),
                    tint = contentColor,
                )
            }
            Column(modifier = Modifier.widthIn(min = 132.dp)) {
                Text(
                    text = stringResource(
                        R.string.ui_voice_message_progress,
                        formatDurationMs(state.positionMs),
                        formatDurationMs(state.display.totalMs),
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    color = contentColor,
                )
                Spacer(modifier = Modifier.height(4.dp))
                LinearProgressIndicator(
                    progress = {
                        if (state.display.totalMs > 0) {
                            (state.positionMs.toFloat() / state.display.totalMs).coerceIn(0f, 1f)
                        } else {
                            0f
                        }
                    },
                    color = contentColor,
                    trackColor = contentColor.copy(alpha = 0.25f),
                    drawStopIndicator = {},
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(3.dp)
                        // Not `contentDescription = ""`: that leaves the node's
                        // progress range in place and TalkBack reads a percentage
                        // over the "0:04 / 0:12" line right above it.
                        .clearAndSetSemantics {},
                )
            }
        }
        if (state.display.failed) {
            Text(
                text = stringResource(R.string.ui_could_not_play_voice_message),
                style = MaterialTheme.typography.bodySmall,
                color = contentColor.copy(alpha = 0.8f),
                modifier = Modifier.padding(top = 2.dp),
            )
        }
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
