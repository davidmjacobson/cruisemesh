package com.cruisemesh.app.chat

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.LocalVoiceMessagePlayback
import com.cruisemesh.app.media.VoiceMessageAudioFocus
import com.cruisemesh.app.media.VoiceMessageAudioPlayer
import com.cruisemesh.app.media.VoiceMessagePlayback
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The bubble half of the reload regression: [VoiceMessagePlaybackTest] pins the
 * player, this pins the fact that the bubble no longer keys anything on the
 * attachment's `ByteArray`. A chat reload hands it a fresh, byte-identical
 * array — which is exactly what used to dispose the player mid-message.
 */
@RunWith(AndroidJUnit4::class)
class VoiceBubblePlaybackUiTest {
    @get:Rule
    val compose = createComposeRule()

    private class FakePlayer : VoiceMessageAudioPlayer {
        override val durationMs = 16_000
        override var positionMs = 0
        var released = false

        override fun start() = Unit
        override fun pause() = Unit
        override fun seekTo(positionMs: Int) {
            this.positionMs = positionMs
        }
        override fun release() {
            released = true
        }

        override fun setListeners(onComplete: () -> Unit, onError: () -> Unit) = Unit
    }

    private object NoFocus : VoiceMessageAudioFocus {
        override fun request(onLoss: () -> Unit) = Unit
        override fun abandon() = Unit
    }

    @Test
    fun aReloadedBlobDoesNotStopAPlayingMessage() {
        val player = FakePlayer()
        val playback = VoiceMessagePlayback(
            focus = NoFocus,
            load = { _, onPrepared -> onPrepared(player) },
        )
        var blob by mutableStateOf(byteArrayOf(1, 2, 3))

        compose.setContent {
            CruiseMeshTheme {
                CompositionLocalProvider(LocalVoiceMessagePlayback provides playback) {
                    AttachmentBubbleContent(
                        attachment = AttachmentPayload(
                            mediaType = AttachmentPayload.MediaType.AUDIO,
                            mimeType = "audio/mp4",
                            durationMs = 16_000,
                            blob = blob,
                        ),
                        messageKey = "111:9",
                        contentColor = Color.Black,
                    )
                }
            }
        }

        compose.onNodeWithContentDescription("Play voice message").performClick()
        compose.onNodeWithContentDescription("Pause voice message").assertIsDisplayed()

        player.positionMs = 7_000
        playback.tick()
        // The reload: same bytes, new instance, as the store hands back every
        // time anything in the chat changes.
        blob = byteArrayOf(1, 2, 3)
        compose.waitForIdle()

        compose.onNodeWithContentDescription("Pause voice message").assertIsDisplayed()
        compose.onNodeWithContentDescription("Voice message position").assertIsDisplayed()
        assertFalse(player.released)
    }

    /**
     * Previews and any future bubble rendered outside a conversation fall back
     * to a player of their own. Nothing to stay continuous with there — this
     * only pins that the fallback composes rather than crashing.
     */
    @Test
    fun aBubbleWithNoConversationAroundItStillRenders() {
        compose.setContent {
            CruiseMeshTheme {
                AttachmentBubbleContent(
                    attachment = AttachmentPayload(
                        mediaType = AttachmentPayload.MediaType.AUDIO,
                        mimeType = "audio/mp4",
                        durationMs = 4_000,
                        blob = byteArrayOf(9),
                    ),
                    messageKey = "111:10",
                    contentColor = Color.Black,
                )
            }
        }

        compose.onNodeWithContentDescription("Voice message position").assertIsDisplayed()
        compose.onNodeWithContentDescription("Play voice message").assertIsDisplayed()
    }
}
