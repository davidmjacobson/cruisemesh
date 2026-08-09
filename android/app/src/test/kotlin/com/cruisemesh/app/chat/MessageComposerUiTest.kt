package com.cruisemesh.app.chat

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertWidthIsAtLeast
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
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

        compose.onNodeWithContentDescription("Hold to talk").assertIsDisplayed()
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

        listOf("Attach photo from library", "Take photo", "Hold to talk").forEach { label ->
            compose.onNodeWithContentDescription(label)
                .assertWidthIsAtLeast(48.dp)
                .assertHeightIsAtLeast(48.dp)
        }
    }

    /**
     * The gesture's meaning lives in `core/src/voice.rs` and is tested there.
     * These cover the wiring: that the composer feeds real pointer events into
     * it and acts on the effect it gets back.
     */
    @Test
    fun holdingTheMicRecordsAndReleasingSends() {
        val recorder = FakeRecorder()

        compose.setContent { ComposerUnderTest(recorder) }

        val mic = compose.onNodeWithContentDescription("Hold to talk")
        mic.performTouchInput { down(center) }
        recorder.clockMs += 2_000
        mic.performTouchInput { up() }

        assertEquals(1, recorder.started)
        assertEquals(1, recorder.stopped)
        assertEquals(0, recorder.cancelled)
    }

    @Test
    fun slidingLeftBeforeReleasingCancelsInsteadOfSending() {
        val recorder = FakeRecorder()

        compose.setContent { ComposerUnderTest(recorder) }

        val mic = compose.onNodeWithContentDescription("Hold to talk")
        mic.performTouchInput {
            down(center)
            moveTo(center + Offset(-400f, 0f))
        }
        recorder.clockMs += 2_000
        mic.performTouchInput { up() }

        assertEquals(1, recorder.started)
        assertEquals(0, recorder.stopped)
        assertEquals(1, recorder.cancelled)
    }

    @Test
    fun aTapTooShortToBeSpeechDiscardsTheRecording() {
        val recorder = FakeRecorder()

        compose.setContent { ComposerUnderTest(recorder) }

        compose.onNodeWithContentDescription("Hold to talk").performTouchInput {
            down(center)
            up()
        }

        assertEquals(1, recorder.started)
        assertEquals(0, recorder.stopped)
        assertEquals(1, recorder.cancelled)
    }

    @Test
    fun aRefusedMicrophoneNeverEntersTheRecordingState() {
        val recorder = FakeRecorder(canStart = false)

        compose.setContent { ComposerUnderTest(recorder) }

        val mic = compose.onNodeWithContentDescription("Hold to talk")
        mic.performTouchInput { down(center) }
        recorder.clockMs += 2_000
        mic.performTouchInput { up() }

        assertEquals(1, recorder.started)
        assertEquals(0, recorder.stopped)
        assertEquals(0, recorder.cancelled)
        // Still the mic, never a recording pill.
        compose.onNodeWithContentDescription("Hold to talk").assertIsDisplayed()
    }

    /**
     * Hold-to-talk is a gesture a switch-access or TalkBack user cannot make,
     * so the mic also carries a plain click action into the hands-free state,
     * where Cancel and Send are ordinary buttons.
     */
    @Test
    fun theMicHasAClickActionForPeopleWhoCannotHold() {
        val recorder = FakeRecorder()

        compose.setContent { ComposerUnderTest(recorder) }

        compose.onNodeWithContentDescription("Hold to talk")
            .performSemanticsAction(SemanticsActions.OnClick)
        assertEquals(1, recorder.started)

        recorder.clockMs += 3_000
        compose.onNodeWithContentDescription("Send voice message").performClick()

        assertEquals(1, recorder.stopped)
        assertEquals(0, recorder.cancelled)
    }

    @Test
    fun aHandsFreeRecordingCanBeCancelledFromThePill() {
        val recorder = FakeRecorder()

        compose.setContent { ComposerUnderTest(recorder) }

        compose.onNodeWithContentDescription("Hold to talk")
            .performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithText("Cancel").performClick()

        assertEquals(1, recorder.started)
        assertEquals(0, recorder.stopped)
        assertEquals(1, recorder.cancelled)
    }

    /**
     * The duration bound only holds if the encoder honoured the bitrate it was
     * asked for. When it did not, the file fills the envelope early and the
     * recording has to end there — not after the user has finished speaking.
     */
    @Test
    fun anEncoderThatOvershootsEndsTheRecordingOnBytes() {
        val recorder = FakeRecorder()

        compose.setContent { ComposerUnderTest(recorder) }

        compose.onNodeWithContentDescription("Hold to talk")
            .performSemanticsAction(SemanticsActions.OnClick)
        assertEquals(1, recorder.started)
        assertEquals(0, recorder.stopped)

        // Far past any plausible budget, however generous.
        recorder.bytes = 4L * 1024 * 1024
        recorder.clockMs += 1_000
        repeat(10) {
            compose.mainClock.advanceTimeBy(200)
            compose.waitForIdle()
        }

        assertEquals(1, recorder.stopped)
        assertEquals(0, recorder.cancelled)
    }

    @Composable
    private fun ComposerUnderTest(recorder: FakeRecorder) {
        CruiseMeshTheme {
            MessageComposer(
                draft = "",
                onDraftChange = {},
                onSend = {},
                hasPendingAttachment = false,
                ownBubbleColor = Color(0xFF236A5B),
                onPickGallery = {},
                onPickCamera = {},
                onStartVoice = {
                    recorder.started += 1
                    recorder.canStart
                },
                onStopVoice = { recorder.stopped += 1 },
                onCancelVoice = { recorder.cancelled += 1 },
                bytesRecorded = { recorder.bytes },
                nowMs = { recorder.clockMs },
            )
        }
    }

    private class FakeRecorder(val canStart: Boolean = true) {
        var started = 0
        var stopped = 0
        var cancelled = 0
        var clockMs = 1_000_000L
        var bytes = 0L
    }
}
