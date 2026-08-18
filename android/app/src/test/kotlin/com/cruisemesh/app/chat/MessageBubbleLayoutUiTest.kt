package com.cruisemesh.app.chat

import android.content.Context
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.R
import com.cruisemesh.app.ui.BubbleGrouping
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.formatConversationTimestamp
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.StoredMessage
import uniffi.cruisemesh_core.coreLegacyDeviceId

@RunWith(AndroidJUnit4::class)
class MessageBubbleLayoutUiTest {
    @get:Rule
    val compose = createComposeRule()
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun timestampIsInsideBubbleAndReactionStraddlesItsBottomEdge() {
        val timestamp = 1_783_608_000_000L
        val message = StoredMessage(
            chatId = byteArrayOf(1),
            senderUserId = byteArrayOf(2),
            lamport = 1u,
            timestamp = timestamp,
            kind = 1u,
            payload = "Save me a seat".toByteArray(),
            senderDeviceId = coreLegacyDeviceId(),
        )

        compose.setContent {
            CruiseMeshTheme {
                MessageBubbleVisual(
                    message = message,
                    isOwn = true,
                    tick = TickStatus.READ,
                    contactColor = null,
                    shape = RoundedCornerShape(20.dp),
                    showTimestamp = true,
                    reactions = listOf(ReactionSummary("❤️", 1, true)),
                    onReact = {},
                )
            }
        }

        val bubble = compose.onNodeWithTag(MESSAGE_BUBBLE_SURFACE_TEST_TAG, useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val time = compose.onNodeWithText(formatConversationTimestamp(context, timestamp))
            .fetchSemanticsNode().boundsInRoot
        val reaction = compose.onNodeWithText("❤️")
            .fetchSemanticsNode().boundsInRoot

        assertTrue("timestamp should start inside the bubble", time.top >= bubble.top)
        assertTrue("timestamp should end inside the bubble", time.bottom <= bubble.bottom)
        assertTrue("reaction should overlap the bubble", reaction.top < bubble.bottom)
        assertTrue("reaction should also remain below the bubble", reaction.bottom > bubble.bottom)
    }

    @Test
    fun lateArrivalExplanationRemainsOutsideTheBubble() {
        val sentAt = 1_783_608_000_000L
        val arrivedAt = sentAt + 3_600_000L
        val message = storedMessage(sentAt)

        compose.setContent {
            CruiseMeshTheme {
                MessageBubble(
                    message = message,
                    isFocused = false,
                    isOwn = false,
                    tick = null,
                    contactColor = Color(0xFF236A5B),
                    grouping = BubbleGrouping(joinsPrevious = false, joinsNext = false),
                    lateArrivalMs = arrivedAt,
                )
            }
        }

        val bubble = compose.onNodeWithTag(MESSAGE_BUBBLE_SURFACE_TEST_TAG, useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val sentTime = compose.onNodeWithText(formatConversationTimestamp(context, sentAt))
            .fetchSemanticsNode().boundsInRoot
        val arrived = compose.onNodeWithText(
            context.getString(R.string.ui_arrived_at, formatConversationTimestamp(context, arrivedAt)),
        ).fetchSemanticsNode().boundsInRoot

        assertTrue("sent timestamp should remain inside the bubble", sentTime.bottom <= bubble.bottom)
        assertTrue("arrival explanation should remain below the bubble", arrived.top >= bubble.bottom)
    }

    @Test
    fun deliveryFailureExplanationRemainsOutsideTheBubble() {
        val sentAt = 1_783_608_000_000L

        compose.setContent {
            CruiseMeshTheme {
                MessageBubble(
                    message = storedMessage(sentAt),
                    isFocused = false,
                    isOwn = true,
                    tick = TickStatus.SENT,
                    contactColor = null,
                    grouping = BubbleGrouping(joinsPrevious = false, joinsNext = false),
                    outboundExpiryMs = 1L,
                )
            }
        }

        val bubble = compose.onNodeWithTag(MESSAGE_BUBBLE_SURFACE_TEST_TAG, useUnmergedTree = true)
            .fetchSemanticsNode().boundsInRoot
        val notice = compose.onNodeWithText(context.getString(R.string.ui_not_delivered))
            .fetchSemanticsNode().boundsInRoot

        assertTrue("delivery failure should remain below the bubble", notice.top >= bubble.bottom)
    }

    private fun storedMessage(timestamp: Long) = StoredMessage(
        chatId = byteArrayOf(1),
        senderUserId = byteArrayOf(2),
        lamport = 1u,
        timestamp = timestamp,
        kind = 1u,
        payload = "Save me a seat".toByteArray(),
        senderDeviceId = coreLegacyDeviceId(),
    )
}
