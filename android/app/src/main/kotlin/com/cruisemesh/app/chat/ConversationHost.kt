package com.cruisemesh.app.chat

import android.content.Context
import android.widget.Toast
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Scaffold
import androidx.compose.material3.ScaffoldDefaults
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.LocalVoiceMessagePlayback
import com.cruisemesh.app.media.rememberVoiceMessagePlayback
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import uniffi.cruisemesh_core.StoredMessage
import java.io.File

/**
 * Shared state-holder for the parts of [ChatScreen] and [GroupChatScreen]
 * that are byte-identical between a 1:1 and a group thread: the long-press
 * focus overlay (which message is focused, and where its bubble sits on
 * screen for [MessageFocusOverlay] to anchor on), the message-info bottom
 * sheet's target, and the FA7 auto-scroll / "new messages" chip machinery
 * driving the reverseLayout [LazyColumn] both screens use. See
 * [rememberConversationHost] for construction and [ConversationHostEffects]
 * for the `LaunchedEffect`s that drive it every recomposition.
 *
 * What's deliberately NOT here: anything that differs between a 1:1 contact
 * and a group (reload-from-store, which fields a screen reloads, sender
 * dispatch) stays in each screen -- see FA15 refactor notes in ChatScreen.kt
 * / GroupChatScreen.kt for the parts left divergent on purpose.
 */
@Stable
class ConversationHost internal constructor(
    val listState: LazyListState,
    private val scrollScope: CoroutineScope,
    val keyboardFreeze: OverlayKeyboardFreeze,
) {
    var focused by mutableStateOf<FocusedMessage?>(null)
        private set

    /** Which message the info bottom sheet is showing, if any. */
    var infoMessage by mutableStateOf<StoredMessage?>(null)
        private set

    var newMessagesAvailable by mutableStateOf(false)
        private set

    private var newestMessageKey: String? = null

    /** Keys of the previous visible list, for spotting messages spliced in above the tail. */
    private var previousKeys: Set<String> = emptySet()

    /**
     * Oldest-first index the chip should jump to, or null to jump to the
     * bottom. Set when a delayed message lands above the tail, where the
     * newest message is not the one the reader is being told about.
     */
    private var insertedJumpIndex: Int? = null

    val overlayOpen: Boolean get() = focused != null

    /**
     * Call before showing [MessageFocusOverlay]: freezes the keyboard/layout
     * (Signal-style, see [OverlayKeyboardFreeze]) and records which bubble is
     * focused and where it sits on screen.
     */
    fun openOverlay(target: MessageTarget, bounds: Rect) {
        keyboardFreeze.onOverlayOpened()
        focused = FocusedMessage(target, bounds)
    }

    fun closeOverlay() {
        focused = null
        keyboardFreeze.onOverlayClosed()
    }

    /**
     * Replaces the focus overlay with Message info without briefly restoring
     * the composer keyboard between the two modal surfaces.
     */
    fun openInfo(message: StoredMessage) {
        infoMessage = message
        focused = null
        keyboardFreeze.onOverlayClosed(shouldRestoreKeyboard = false)
    }

    fun closeInfo() {
        infoMessage = null
    }

    /** Resolves the currently-focused message out of [visibleMessages], or null if it's vanished (e.g. deleted). */
    fun resolveFocusedMessage(visibleMessages: List<StoredMessage>): StoredMessage? {
        val target = focused?.target ?: return null
        return visibleMessages.firstOrNull {
            MessageTarget(it.senderUserId, it.lamport, it.kind).stableKey == target.stableKey
        }
    }

    /** Animates the reverseLayout list to [message], mapping its oldest-first index to the reversed display index. */
    fun scrollToMessage(visibleMessages: List<StoredMessage>, message: StoredMessage) {
        val oldestFirstIndex = visibleMessages.indexOfFirst { messageStableKey(it) == messageStableKey(message) }
        if (oldestFirstIndex < 0) return
        val displayIndex = visibleMessages.lastIndex - oldestFirstIndex
        scrollScope.launch { listState.animateScrollToItem(displayIndex) }
    }

    /**
     * [NewMessagesChip]'s onClick: jump to what the chip is announcing and
     * dismiss it. Usually that is the newest message at the bottom; when a
     * delayed message was spliced into history it is that message instead,
     * which is the only way the reader would ever find it.
     */
    fun scrollToBottomAndClearChip() {
        val target = insertedJumpIndex
        scrollScope.launch { listState.animateScrollToItem(target ?: 0) }
        newMessagesAvailable = false
        insertedJumpIndex = null
    }

    /**
     * FA7: only auto-scroll to the bottom when the reader is already there
     * (or the arriving message is their own send) -- otherwise a history
     * backfill or an incoming message while reading up-thread would yank the
     * view. See [ChatScrollLogic] for the pure decision. Run from a
     * `LaunchedEffect(visibleMessages)` -- see [ConversationHostEffects].
     */
    suspend fun onVisibleMessagesChanged(
        visibleMessages: List<StoredMessage>,
        ownUserId: ByteArray,
        lateArrivalKeys: Set<String> = emptySet(),
    ) {
        val currentNewestKey = visibleMessages.lastOrNull()?.let(::messageStableKey)
        val isNewestOwn = visibleMessages.lastOrNull()?.senderUserId?.contentEquals(ownUserId) == true
        val currentKeys = visibleMessages.map(::messageStableKey)
        val insertedIndex = ChatScrollLogic.oldestInsertedIndex(previousKeys, currentKeys, lateArrivalKeys)
        when (
            ChatScrollLogic.decide(
                previousNewestKey = newestMessageKey,
                currentNewestKey = currentNewestKey,
                firstVisibleItemIndex = listState.firstVisibleItemIndex,
                isNewestOwnMessage = isNewestOwn,
                insertedAboveTail = insertedIndex != null,
            )
        ) {
            ChatScrollLogic.Decision.AUTO_SCROLL -> {
                // reverseLayout start is the bottom; pin the newest message there.
                listState.scrollToItem(0)
                newMessagesAvailable = false
                insertedJumpIndex = null
            }
            ChatScrollLogic.Decision.SHOW_NEW_MESSAGES_CHIP -> newMessagesAvailable = true
            ChatScrollLogic.Decision.SHOW_INSERTED_ABOVE_CHIP -> {
                newMessagesAvailable = true
                // Oldest-first index -> reverseLayout display index.
                insertedJumpIndex = insertedIndex?.let { visibleMessages.lastIndex - it }
            }
            ChatScrollLogic.Decision.NONE -> {}
        }
        newestMessageKey = currentNewestKey ?: newestMessageKey
        previousKeys = currentKeys.toSet()
    }

    /**
     * Clears the chip once the reader scrolls back to the bottom themselves.
     * Run from a `LaunchedEffect(listState)`.
     *
     * Reaching the bottom does not clear a chip pointing at a message spliced
     * into history: that message is above the reader, so scrolling down is
     * not them having seen it.
     */
    suspend fun watchScrollForChipClear() {
        snapshotFlow { listState.firstVisibleItemIndex }.collect { index ->
            if (index <= 1 && insertedJumpIndex == null) newMessagesAvailable = false
        }
    }
}

/**
 * Builds a [ConversationHost] keyed on [chatId] (a contact's userId or a
 * group's id), so switching threads resets overlay/scroll-chip state exactly
 * like the individual `remember(contact.userId)` / `remember(group.id)`
 * calls this replaces. [listState], [scrollScope] and [keyboardFreeze]
 * themselves are intentionally NOT re-created on a chatId change, matching
 * the pre-refactor behavior where those three were never keyed either.
 */
@Composable
fun rememberConversationHost(chatId: ByteArray): ConversationHost {
    val listState = rememberLazyListState()
    val scrollScope = rememberCoroutineScope()
    val keyboardFreeze = rememberOverlayKeyboardFreeze()
    return remember(chatId) {
        ConversationHost(listState = listState, scrollScope = scrollScope, keyboardFreeze = keyboardFreeze)
    }
}

/**
 * Wires the three `LaunchedEffect`s [ChatScreen] and [GroupChatScreen] both
 * ran around their overlay/scroll state: releasing the keyboard freeze once
 * the overlay closes, the FA7 auto-scroll/"new messages" chip decision on
 * every visible-list change, and clearing that chip once the reader scrolls
 * back to the bottom themselves.
 */
@Composable
fun ConversationHostEffects(
    host: ConversationHost,
    visibleMessages: List<StoredMessage>,
    ownUserId: ByteArray,
    lateArrivalKeys: Set<String> = emptySet(),
) {
    LaunchedEffect(host.overlayOpen) {
        if (!host.overlayOpen) {
            host.keyboardFreeze.releaseWhenKeyboardReturns()
        }
    }
    LaunchedEffect(visibleMessages, lateArrivalKeys) {
        host.onVisibleMessagesChanged(visibleMessages, ownUserId, lateArrivalKeys)
    }
    LaunchedEffect(host.listState) {
        host.watchScrollForChipClear()
    }
}

/**
 * The reverseLayout [LazyColumn] scaffolding [ChatScreen] and
 * [GroupChatScreen] both build around their message list: the outer
 * `BoxWithConstraints` (needed to know the viewport height for
 * [OverlayKeyboardFreeze]), a [Scaffold] with the caller's [topBar] and a
 * snackbar host, the adjustResize-aware bottom-inset tracking that feeds
 * [OverlayKeyboardFreeze], and the list itself stacked with [NewMessagesChip].
 *
 * [listContent] supplies just the `itemsIndexed(...)` call for the caller's
 * own bubble rendering (1:1 and group bubbles differ too much to share).
 * [belowList] supplies whatever sits under the list -- the pending-photo
 * card, reply preview and [MessageComposer] -- in each screen's own order
 * (the two screens order the photo card and reply preview differently; see
 * the FA15 refactor notes for why that's preserved rather than unified).
 * [overlays] supplies dialogs and [MessageFocusOverlay], rendered as
 * siblings of the [Scaffold] inside the same `BoxWithConstraints` so they
 * stack above it, exactly as before this was factored out.
 */
@Composable
fun ConversationScaffold(
    host: ConversationHost,
    topBar: @Composable () -> Unit,
    snackbarHostState: SnackbarHostState,
    listContent: LazyListScope.() -> Unit,
    belowList: @Composable ColumnScope.() -> Unit,
    overlays: @Composable () -> Unit = {},
) {
    // One voice player for the whole conversation, owned here rather than by
    // the bubble that starts a message: this outlives both a store reload and
    // the LazyColumn disposing a scrolled-away bubble, either of which used to
    // cut a message off mid-sentence (see [VoiceMessagePlayback]). It covers
    // the overlays too, so a message playing in the list keeps playing under
    // the focus overlay.
    CompositionLocalProvider(LocalVoiceMessagePlayback provides rememberVoiceMessagePlayback()) {
        ConversationScaffoldContent(host, topBar, snackbarHostState, listContent, belowList, overlays)
    }
}

@Composable
private fun ConversationScaffoldContent(
    host: ConversationHost,
    topBar: @Composable () -> Unit,
    snackbarHostState: SnackbarHostState,
    listContent: LazyListScope.() -> Unit,
    belowList: @Composable ColumnScope.() -> Unit,
    overlays: @Composable () -> Unit,
) {
    val density = LocalDensity.current
    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        val viewportHeightPx = with(density) { maxHeight.toPx() }
        Scaffold(
            topBar = topBar,
            snackbarHost = { SnackbarHost(snackbarHostState) },
            // The app is edge-to-edge (enableEdgeToEdge in MainActivity), so the
            // manifest's adjustResize declaration only forbids *panning* -- the
            // system never resizes the window for the keyboard; it hands the IME
            // to the app as an inset to consume. Union it into the content
            // insets so the composer rises with the keyboard. A union (per-side
            // max), not a sum: an open keyboard already covers the navigation
            // bar, so stacking the two would double-pad.
            contentWindowInsets = ScaffoldDefaults.contentWindowInsets.union(WindowInsets.ime),
        ) { innerPadding ->
            // innerPadding's bottom carries the IME (see contentWindowInsets
            // above), so the usable content edge tracked here rises and falls
            // with the keyboard; OverlayKeyboardFreeze pins that edge while the
            // keyboard animates away and back under the focus overlay. Deriving
            // it from Scaffold's own applied padding, never from a raw
            // WindowInsets.ime reading, is what keeps the freeze's two numbers
            // from diverging (see OverlayKeyboardFreeze's history note).
            val bottomInsetPx = with(density) { innerPadding.calculateBottomPadding().toPx() }
            val contentBottomPx = viewportHeightPx - bottomInsetPx
            val imeVisible = WindowInsets.ime.getBottom(density) > 0
            SideEffect { host.keyboardFreeze.trackLiveContentBottom(contentBottomPx, imeVisible) }
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .padding(bottom = with(density) { host.keyboardFreeze.extraBottomPx.toDp() })
                    .padding(horizontal = 16.dp),
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                ) {
                    LazyColumn(
                        state = host.listState,
                        reverseLayout = true,
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(vertical = 8.dp),
                        content = listContent,
                    )

                    if (host.newMessagesAvailable) {
                        NewMessagesChip(
                            onClick = { host.scrollToBottomAndClearChip() },
                            modifier = Modifier
                                .align(Alignment.BottomCenter)
                                .padding(bottom = 12.dp),
                        )
                    }
                } // Box (LazyColumn + New messages chip)

                belowList()
            }
        }

        overlays()
    } // Box
}

/** Default snackbar copy for a failed send -- the message itself stays visible in the composer/list either way. */
internal const val SEND_FAILURE_MESSAGE = "Couldn't send. Your message is still here."

/** Shows [message] in [snackbarHostState] via [scope]. Shared send-failure snackbar for [ChatScreen] and [GroupChatScreen]. */
fun showSendFailureSnackbar(
    scope: CoroutineScope,
    snackbarHostState: SnackbarHostState,
    message: String = SEND_FAILURE_MESSAGE,
) {
    scope.launch { snackbarHostState.showSnackbar(message) }
}

/**
 * Stages a compressed photo for sending via [onStaged], or -- if compression
 * failed -- toasts the same "could not prepare" copy [ChatScreen] and
 * [GroupChatScreen] have always shown.
 */
fun stagePhotoOrWarn(context: Context, jpeg: ByteArray?, onStaged: (ByteArray) -> Unit) {
    if (jpeg == null) {
        Toast.makeText(
            context,
            context.getString(R.string.ui_could_not_prepare_photo),
            Toast.LENGTH_SHORT,
        ).show()
        return
    }
    onStaged(jpeg)
}

/**
 * Reads [file]'s bytes for an about-to-send voice message, deleting the temp
 * recording either way, and validates size against
 * [AttachmentPayload.MAX_BLOB_BYTES]. Toasts the same "could not save" /
 * "too large" copy [ChatScreen] and [GroupChatScreen] have always shown on
 * failure and returns null so the caller just bails out.
 */
fun readVoiceMemoBytes(context: Context, file: File): ByteArray? {
    val bytes = try {
        file.readBytes()
    } catch (_: Exception) {
        null
    }
    file.delete()
    if (bytes == null || bytes.isEmpty()) {
        Toast.makeText(
            context,
            context.getString(R.string.ui_could_not_save_voice_message),
            Toast.LENGTH_SHORT,
        ).show()
        return null
    }
    if (bytes.size > AttachmentPayload.MAX_BLOB_BYTES) {
        Toast.makeText(
            context,
            context.getString(R.string.ui_voice_message_too_large),
            Toast.LENGTH_SHORT,
        ).show()
        return null
    }
    return bytes
}
