package com.cruisemesh.app.chat

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertWidthIsAtLeast
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MessageComposerUiTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun typingSwitchesFromVoiceToSendAndDispatchesOnce() {
        var draft by mutableStateOf("")
        var sent = 0

        compose.setContent {
            CruiseMeshTheme {
                MessageComposer(
                    draft = draft,
                    onDraftChange = { draft = it },
                    onSend = { sent += 1 },
                    hasPendingAttachment = false,
                    ownBubbleColor = Color(0xFF236A5B),
                    onPickGallery = {},
                    onPickCamera = {},
                    onStartVoice = { true },
                    onStopVoice = {},
                    onCancelVoice = {},
                )
            }
        }

        compose.onNodeWithContentDescription("Hold to record a voice memo").assertIsDisplayed()
        compose.onNode(hasSetTextAction()).performTextInput("Meet by the pool")
        compose.onNodeWithContentDescription("Send")
            .assertIsDisplayed()
            .assertWidthIsAtLeast(48.dp)
            .assertHeightIsAtLeast(48.dp)
            .performClick()

        assertEquals("Meet by the pool", draft)
        assertEquals(1, sent)
    }

    @Test
    fun attachmentWithoutCaptionIsStillSendable() {
        var sent = 0

        compose.setContent {
            CruiseMeshTheme {
                MessageComposer(
                    draft = "",
                    onDraftChange = {},
                    onSend = { sent += 1 },
                    hasPendingAttachment = true,
                    ownBubbleColor = Color(0xFF236A5B),
                    onPickGallery = {},
                    onPickCamera = {},
                    onStartVoice = { true },
                    onStopVoice = {},
                    onCancelVoice = {},
                )
            }
        }

        compose.onNodeWithContentDescription("Send").performClick()
        assertEquals(1, sent)
    }

    @Test
    fun customComposerActionsMeetMinimumTouchTarget() {
        compose.setContent {
            CruiseMeshTheme {
                MessageComposer(
                    draft = "",
                    onDraftChange = {},
                    onSend = {},
                    hasPendingAttachment = false,
                    ownBubbleColor = Color(0xFF236A5B),
                    onPickGallery = {},
                    onPickCamera = {},
                    onStartVoice = { false },
                    onStopVoice = {},
                    onCancelVoice = {},
                )
            }
        }

        listOf("Attach photo from library", "Take photo", "Hold to record a voice memo").forEach { label ->
            compose.onNodeWithContentDescription(label)
                .assertWidthIsAtLeast(48.dp)
                .assertHeightIsAtLeast(48.dp)
        }
    }
}
