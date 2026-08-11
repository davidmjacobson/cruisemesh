package com.cruisemesh.app.media

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.MediaPlayer
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/** How often the progress bar re-reads the decoder's position. */
private const val PROGRESS_TICK_MS = 100L

/**
 * The conversation's voice-message player, for bubbles nested too deep to take
 * it as a parameter.
 *
 * Null outside a conversation (a preview, a lone bubble in a test), where a
 * bubble falls back to a player of its own: continuity across reloads and
 * scrolling is a conversation concern, and there is no conversation there.
 */
val LocalVoiceMessagePlayback = staticCompositionLocalOf<VoiceMessagePlayback?> { null }

/**
 * A [VoiceMessagePlayback] wired to `MediaPlayer` and this device's audio
 * focus, ticking the progress bar while something plays and releasing the
 * decoder when the screen goes.
 *
 * Call this at the conversation level, above the message list — see
 * [VoiceMessagePlayback] for why the bubble is the wrong place for it.
 */
@Composable
fun rememberVoiceMessagePlayback(): VoiceMessagePlayback {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val playback = remember(context) {
        VoiceMessagePlayback(
            focus = SystemVoiceMessageAudioFocus(
                context.getSystemService(Context.AUDIO_SERVICE) as AudioManager,
            ),
            load = { blob, onPrepared ->
                scope.launch {
                    // Writing the blob to disk and MediaPlayer.prepare() (a
                    // blocking decode of the audio headers) both used to run on
                    // the main thread in the bubble's click handler (FA11).
                    val prepared = withContext(Dispatchers.IO) { openMediaPlayer(context, blob) }
                    onPrepared(prepared)
                }
            },
        )
    }

    // Only while something plays: an idle conversation must not wake up ten
    // times a second to ask a decoder that isn't there.
    val playing = playback.isPlaying
    LaunchedEffect(playback, playing) {
        while (playing) {
            playback.tick()
            delay(PROGRESS_TICK_MS)
        }
    }

    DisposableEffect(playback) {
        onDispose { playback.release() }
    }

    return playback
}

private fun openMediaPlayer(context: Context, blob: ByteArray): VoiceMessageAudioPlayer? = try {
    val temp = File(context.cacheDir, "play-${System.currentTimeMillis()}.m4a")
    temp.writeBytes(blob)
    val player = MediaPlayer()
    player.setAudioAttributes(voicePlaybackAttributes())
    player.setDataSource(temp.absolutePath)
    player.prepare()
    MediaPlayerVoiceMessageAudioPlayer(player, temp)
} catch (_: Exception) {
    null
}

/**
 * Media usage, speech content — so another app's music ducks under a message
 * and comes back afterwards, the same way the iOS spoken-audio session behaves.
 *
 * Deliberately not a communication usage: that asks the platform for a
 * communication route, which on some devices pulls a connected headset onto its
 * hands-free profile and fights the same radio the mesh is using. Voice
 * playback on this project is media, and the mesh keeps running through every
 * audio state.
 */
private fun voicePlaybackAttributes(): AudioAttributes = AudioAttributes.Builder()
    .setUsage(AudioAttributes.USAGE_MEDIA)
    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
    .build()

/**
 * `MediaPlayer` behind the [VoiceMessageAudioPlayer] the conversation talks to.
 *
 * Every call is guarded: a player that errored is in the Error state, where
 * every later call throws, and a chat screen must not crash because a decoder
 * gave up on one message.
 */
private class MediaPlayerVoiceMessageAudioPlayer(
    private val player: MediaPlayer,
    private val backingFile: File,
) : VoiceMessageAudioPlayer {
    private var released = false

    override val durationMs: Int
        get() = if (released) 0 else runCatching { player.duration }.getOrDefault(0)

    override val positionMs: Int
        get() = if (released) 0 else runCatching { player.currentPosition.coerceAtLeast(0) }.getOrDefault(0)

    override fun start() {
        if (!released) runCatching { player.start() }
    }

    override fun pause() {
        if (!released) runCatching { player.pause() }
    }

    override fun release() {
        if (released) return
        released = true
        runCatching { player.stop() }
        runCatching { player.release() }
        backingFile.delete()
    }

    override fun setListeners(onComplete: () -> Unit, onError: () -> Unit) {
        player.setOnCompletionListener { onComplete() }
        player.setOnErrorListener { _, _, _ ->
            onError()
            true
        }
    }
}

/**
 * Ducks whatever else is playing for the length of a message.
 *
 * Playback goes ahead whether or not focus is granted — the user asked for it,
 * and a denied request is only a matter of manners toward other apps.
 */
private class SystemVoiceMessageAudioFocus(
    private val audioManager: AudioManager,
) : VoiceMessageAudioFocus {
    private var granted: AudioFocusRequest? = null

    override fun request(onLoss: () -> Unit) {
        if (granted != null) return
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK)
            .setAudioAttributes(voicePlaybackAttributes())
            .setOnAudioFocusChangeListener { change ->
                if (change == AudioManager.AUDIOFOCUS_LOSS ||
                    change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT
                ) {
                    onLoss()
                }
            }
            .build()
        if (audioManager.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            granted = request
        }
    }

    override fun abandon() {
        granted?.let { audioManager.abandonAudioFocusRequest(it) }
        granted = null
    }
}
