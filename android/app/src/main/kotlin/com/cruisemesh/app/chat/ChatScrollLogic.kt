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
 *
 * A message carried for hours arrives stamped with its send time and so lands
 * *above* replies sent while it was in flight, leaving the tail untouched.
 * Comparing tails alone therefore returned [Decision.NONE] for it: the unread
 * badge counted it, but the thread never moved and a reader sitting at the
 * bottom saw nothing at all. [Decision.SHOW_INSERTED_ABOVE_CHIP] covers that.
 *
 * The gate is the core's late-arrival flag (`core/src/late_arrival.rs`), not
 * an unread delta. Unread would be the more natural signal and cannot be used
 * here: while a chat is on screen `InboundEnvelopeProcessor` records the read
 * receipt as the message lands, so by the time the list reloads the count has
 * already returned to where it was -- the delta is never observable in the
 * one case this exists for. The flag is a good stand-in because it carries
 * the same "displaced by something already here" test that makes an insert
 * worth mentioning at all, and because a chip is a far milder response than
 * the auto-scroll FA7 was protecting the reader from: nothing moves unless
 * they tap it.
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

        /**
         * An unread message landed above the tail -- a delayed delivery
         * spliced into history. Shown even to a reader sitting at the bottom,
         * because the new content is above them and nothing else on screen
         * moved.
         */
        SHOW_INSERTED_ABOVE_CHIP,
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
     * @param insertedAboveTail whether a message the core flagged as a late
     *   arrival appeared above the tail in this update -- see
     *   [oldestInsertedIndex] for locating it.
     */
    fun decide(
        previousNewestKey: String?,
        currentNewestKey: String?,
        firstVisibleItemIndex: Int,
        isNewestOwnMessage: Boolean,
        insertedAboveTail: Boolean = false,
    ): Decision {
        if (currentNewestKey == null) {
            return Decision.NONE
        }
        if (currentNewestKey == previousNewestKey) {
            return if (insertedAboveTail) Decision.SHOW_INSERTED_ABOVE_CHIP else Decision.NONE
        }
        return if (firstVisibleItemIndex <= NEAR_BOTTOM_INDEX || isNewestOwnMessage) {
            Decision.AUTO_SCROLL
        } else {
            Decision.SHOW_NEW_MESSAGES_CHIP
        }
    }

    /**
     * Index into the oldest-first visible list of the oldest message that is
     * both new to this update and flagged as a late arrival -- where the chip
     * should land the reader.
     *
     * Null on the first load (everything is "new", and there is nothing to
     * explain yet) and whenever nothing qualifies, in which case the chip
     * keeps its existing jump-to-bottom behaviour.
     */
    fun oldestInsertedIndex(
        previousKeys: Set<String>,
        currentKeys: List<String>,
        lateArrivalKeys: Set<String>,
    ): Int? {
        if (previousKeys.isEmpty()) return null
        val index = currentKeys.indexOfFirst { it !in previousKeys && it in lateArrivalKeys }
        return index.takeIf { it >= 0 }
    }
}
