package com.cruisemesh.app.chat

/**
 * Pure scroll-on-new-message decision for the reversed (newest-at-index-0)
 * chat `LazyColumn` (FA7). The old behavior --
 * `LaunchedEffect(visibleMessages.size) { listState.scrollToItem(0) }` --
 * fired on *any* size change, so digest-sync backfill of older history
 * yanked a reader who had scrolled up right back to the bottom.
 *
 * [decide] only reacts when the newest message actually changed (a real
 * arrival, not a backfill of older messages, which changes list size without
 * changing what's newest) and only auto-scrolls when the reader is already
 * near the bottom or the new message is their own send; otherwise it signals
 * that a "New messages" affordance should appear instead of yanking the view.
 */
object ChatScrollLogic {
    /** How close to the bottom (`LazyListState.firstVisibleItemIndex`) still counts as "at the bottom". */
    private const val NEAR_BOTTOM_INDEX = 1

    enum class Decision {
        /** Nothing new at the tail -- e.g. a pure history backfill. Leave scroll position alone. */
        NONE,

        /** A new message arrived and the reader was at/near the bottom, or it's their own send. */
        AUTO_SCROLL,

        /** A new message arrived while the reader was scrolled up reading history. */
        SHOW_NEW_MESSAGES_CHIP,
    }

    /**
     * @param previousNewestKey stable key ([messageStableKey]) of the newest
     *   visible message before this update, or null if there was none.
     * @param currentNewestKey stable key of the newest visible message now,
     *   or null if the chat is empty.
     * @param firstVisibleItemIndex `listState.firstVisibleItemIndex` at the
     *   moment the new list landed (index 0 is the bottom in reverseLayout).
     * @param isNewestOwnMessage whether the current newest message was sent
     *   by the local user.
     */
    fun decide(
        previousNewestKey: String?,
        currentNewestKey: String?,
        firstVisibleItemIndex: Int,
        isNewestOwnMessage: Boolean,
    ): Decision {
        if (currentNewestKey == null || currentNewestKey == previousNewestKey) {
            return Decision.NONE
        }
        return if (firstVisibleItemIndex <= NEAR_BOTTOM_INDEX || isNewestOwnMessage) {
            Decision.AUTO_SCROLL
        } else {
            Decision.SHOW_NEW_MESSAGES_CHIP
        }
    }
}
