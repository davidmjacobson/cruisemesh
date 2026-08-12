package com.cruisemesh.app.media

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * A prepared decoder for one voice message. The real one wraps `MediaPlayer`
 * (see `VoiceMessageAudio.kt`); tests supply a fake, which is the only reason
 * this interface exists.
 */
interface VoiceMessageAudioPlayer {
    /** What the decoder found in the blob. 0 when it could not tell. */
    val durationMs: Int

    /** Milliseconds played so far. */
    val positionMs: Int

    fun start()

    fun pause()

    /** Releases the decoder and any temp file behind it. Idempotent. */
    fun release()

    /** Called when the message finishes, and when the decoder gives up. */
    fun setListeners(onComplete: () -> Unit, onError: () -> Unit)
}

/**
 * Ducking for the length of a message: media/speech usage, transient gain, so
 * another app's music comes back afterwards.
 *
 * Focus is about speakers and never about the mesh. `MeshService` has never
 * listened to audio focus and must not start — a paused voice message is a
 * speaker decision, and the radios keep running through all of it.
 */
interface VoiceMessageAudioFocus {
    /** Asks for transient ducking focus. [onLoss] pauses playback if it fires. */
    fun request(onLoss: () -> Unit)

    fun abandon()
}

/** What one voice bubble needs to draw itself. */
data class VoiceBubbleState(
    val isPlaying: Boolean,
    val positionMs: Int,
    val display: VoicePlaybackDisplay,
)

/**
 * Owns voice-message playback for a whole conversation, above the message list.
 *
 * It lives above the list on purpose. Playback state used to sit inside the
 * bubble composable, keyed on the attachment's `ByteArray`, and a `ByteArray`
 * compares by identity: every chat reload re-reads the message from the store
 * and hands the bubble a fresh, byte-identical array, which re-keyed the whole
 * bubble and disposed the player mid-message. Any inbound message, receipt or
 * reaction in the chat — or the listener's own first keystroke, which announces
 * a draft — silently stopped playback and reset the bar to 0:00. A minute-long
 * message in a live conversation could take several tries to hear to the end.
 * Two things follow from hoisting it, and both are the point:
 *
 * - Reloads no longer touch playback: the message's identity is its
 *   sender/lamport key, not the array instance the store just handed back.
 * - Neither does scrolling. A bubble that scrolls out of a `LazyColumn` is
 *   disposed, so the message the user is listening to used to stop when new
 *   arrivals pushed it off screen.
 *
 * Holding it here also makes "one message at a time" true rather than hoped
 * for: starting a second message stops the first, matching the iOS bubble,
 * which takes the shared audio session for the same reason.
 *
 * Not a `ViewModel`: nothing here survives the screen, and the decoder must be
 * released when it goes.
 */
@Stable
class VoiceMessagePlayback(
    private val focus: VoiceMessageAudioFocus,
    /**
     * Decodes and prepares a blob off the main thread, then delivers the
     * player (or null, when it cannot be played) back on it.
     */
    private val load: (ByteArray, (VoiceMessageAudioPlayer?) -> Unit) -> Unit,
) {
    /** The message currently loaded in the decoder, playing or paused. */
    var activeKey by mutableStateOf<String?>(null)
        private set

    var isPlaying by mutableStateOf(false)
        private set

    private var positionMs by mutableIntStateOf(0)
    private var activeDisplay by mutableStateOf(VoicePlaybackDisplay(totalMs = 0, failed = false))

    /**
     * The message whose last attempt failed, if any. One at a time: a failure
     * is about the attempt, and starting another message is a new attempt.
     */
    private var failedKey by mutableStateOf<String?>(null)

    /** Set while a blob is being decoded, so a double-tap cannot start two. */
    private var loadingKey by mutableStateOf<String?>(null)

    private var player: VoiceMessageAudioPlayer? = null

    fun stateFor(key: String, manifestDurationMs: Int): VoiceBubbleState = if (key == activeKey) {
        VoiceBubbleState(isPlaying = isPlaying, positionMs = positionMs, display = activeDisplay)
    } else {
        val display = VoicePlaybackDisplay.initial(manifestDurationMs)
        VoiceBubbleState(
            isPlaying = false,
            positionMs = 0,
            display = if (key == failedKey) display.withFailure() else display,
        )
    }

    /**
     * Play [key], pause it if it is already playing, or resume it where it
     * stopped. Starting a different message stops whatever was playing.
     */
    fun toggle(key: String, blob: ByteArray, manifestDurationMs: Int) {
        val current = player
        when {
            key == activeKey && isPlaying -> pause()
            key == activeKey && current != null -> resume(current)
            key == loadingKey -> Unit
            else -> start(key, blob, manifestDurationMs)
        }
    }

    private fun start(key: String, blob: ByteArray, manifestDurationMs: Int) {
        releasePlayer()
        failedKey = null
        loadingKey = key
        load(blob) { prepared ->
            // A newer tap (or a release) won the race while this decoded.
            if (loadingKey != key) {
                prepared?.release()
                return@load
            }
            loadingKey = null
            if (prepared == null) {
                failedKey = key
                return@load
            }
            prepared.setListeners(
                // Both guarded on still being the current decoder: a message
                // that finishes (or gives up) after the user has already
                // started another one must not tear the new one down.
                onComplete = {
                    // A MediaPlayer is a hardware-codec instance from a small
                    // global pool; a finished message hands its decoder back
                    // rather than holding one open per played bubble.
                    if (player === prepared) releasePlayer()
                },
                onError = {
                    if (player === prepared) {
                        releasePlayer()
                        failedKey = key
                    }
                },
            )
            player = prepared
            activeKey = key
            activeDisplay = VoicePlaybackDisplay.initial(manifestDurationMs)
                .withDecoderDuration(prepared.durationMs)
            positionMs = 0
            focus.request(::pause)
            isPlaying = true
            prepared.start()
        }
    }

    private fun resume(current: VoiceMessageAudioPlayer) {
        failedKey = null
        focus.request(::pause)
        isPlaying = true
        current.start()
    }

    /**
     * Pauses and hands focus back, keeping the decoder so resuming picks up
     * where it stopped instead of decoding the blob again.
     */
    fun pause() {
        val current = player ?: return
        if (!isPlaying) return
        isPlaying = false
        current.pause()
        positionMs = current.positionMs
        focus.abandon()
    }

    /** Polls the decoder for the progress bar. Driven by the screen's ticker. */
    fun tick() {
        val current = player ?: return
        if (isPlaying) {
            positionMs = current.positionMs
        }
    }

    /** Screen teardown: stop everything and let the decoder go. */
    fun release() {
        loadingKey = null
        releasePlayer()
    }

    private fun releasePlayer() {
        player?.release()
        player = null
        activeKey = null
        isPlaying = false
        positionMs = 0
        activeDisplay = VoicePlaybackDisplay(totalMs = 0, failed = false)
        focus.abandon()
    }
}
