package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

class ChatScrollLogicTest {
    @Test
    fun noNewestChange_isNone() {
        // A history backfill changes list size/contents but not the tail --
        // e.g. older messages prepended, newest unchanged.
        assertEquals(
            ChatScrollLogic.Decision.NONE,
            ChatScrollLogic.decide(
                previousNewestKey = "a",
                currentNewestKey = "a",
                firstVisibleItemIndex = 5,
                isNewestOwnMessage = false,
            ),
        )
    }

    @Test
    fun emptyChat_isNone() {
        assertEquals(
            ChatScrollLogic.Decision.NONE,
            ChatScrollLogic.decide(
                previousNewestKey = null,
                currentNewestKey = null,
                firstVisibleItemIndex = 0,
                isNewestOwnMessage = false,
            ),
        )
    }

    @Test
    fun firstMessageEverLoaded_atBottom_autoScrolls() {
        // Initial screen open: previousNewestKey is null, reader is at the
        // bottom by definition (listState starts at index 0).
        assertEquals(
            ChatScrollLogic.Decision.AUTO_SCROLL,
            ChatScrollLogic.decide(
                previousNewestKey = null,
                currentNewestKey = "a",
                firstVisibleItemIndex = 0,
                isNewestOwnMessage = false,
            ),
        )
    }

    @Test
    fun newIncomingMessage_atBottom_autoScrolls() {
        assertEquals(
            ChatScrollLogic.Decision.AUTO_SCROLL,
            ChatScrollLogic.decide(
                previousNewestKey = "a",
                currentNewestKey = "b",
                firstVisibleItemIndex = 0,
                isNewestOwnMessage = false,
            ),
        )
    }

    @Test
    fun newIncomingMessage_nearBottom_autoScrolls() {
        assertEquals(
            ChatScrollLogic.Decision.AUTO_SCROLL,
            ChatScrollLogic.decide(
                previousNewestKey = "a",
                currentNewestKey = "b",
                firstVisibleItemIndex = 1,
                isNewestOwnMessage = false,
            ),
        )
    }

    @Test
    fun newIncomingMessage_scrolledUpReadingHistory_showsChip() {
        assertEquals(
            ChatScrollLogic.Decision.SHOW_NEW_MESSAGES_CHIP,
            ChatScrollLogic.decide(
                previousNewestKey = "a",
                currentNewestKey = "b",
                firstVisibleItemIndex = 40,
                isNewestOwnMessage = false,
            ),
        )
    }

    @Test
    fun ownNewMessage_scrolledUp_stillAutoScrolls() {
        // Accept criterion: sending your own message always scrolls to it,
        // even if you'd scrolled up to read history first.
        assertEquals(
            ChatScrollLogic.Decision.AUTO_SCROLL,
            ChatScrollLogic.decide(
                previousNewestKey = "a",
                currentNewestKey = "b",
                firstVisibleItemIndex = 40,
                isNewestOwnMessage = true,
            ),
        )
    }
}
