package com.cruisemesh.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private const val KEY_A = "111:9"
private const val KEY_B = "111:10"

class VoiceMessagePlaybackTest {
    private class FakePlayer(
        override val durationMs: Int = 16_000,
        override var positionMs: Int = 0,
    ) : VoiceMessageAudioPlayer {
        var starts = 0
        var pauses = 0
        var released = false
        var onComplete: () -> Unit = {}
        var onError: () -> Unit = {}

        override fun start() {
            starts += 1
        }

        override fun pause() {
            pauses += 1
        }

        override fun release() {
            released = true
        }

        override fun setListeners(onComplete: () -> Unit, onError: () -> Unit) {
            this.onComplete = onComplete
            this.onError = onError
        }
    }

    private class FakeFocus : VoiceMessageAudioFocus {
        var held = false
        var onLoss: () -> Unit = {}

        override fun request(onLoss: () -> Unit) {
            held = true
            this.onLoss = onLoss
        }

        override fun abandon() {
            held = false
        }
    }

    /** Loads synchronously, handing out [players] in order; null means "won't decode". */
    private class Loader(private vararg val players: VoiceMessageAudioPlayer?) {
        var loads = 0
        val load: (ByteArray, (VoiceMessageAudioPlayer?) -> Unit) -> Unit = { _, onPrepared ->
            val next = players.getOrNull(loads)
            loads += 1
            onPrepared(next)
        }
    }

    private fun playback(loader: Loader, focus: FakeFocus = FakeFocus()) =
        VoiceMessagePlayback(focus = focus, load = loader.load)

    /**
     * The bug this class exists for: every chat reload hands the bubble a
     * fresh, byte-identical `ByteArray`, and playback used to be keyed on that
     * array's identity, so any inbound message, receipt, reaction or first
     * keystroke stopped the message mid-sentence.
     */
    @Test
    fun `a reload that hands back an equal but fresh blob does not disturb playback`() {
        val player = FakePlayer()
        val loader = Loader(player)
        val playback = playback(loader)
        val blob = byteArrayOf(1, 2, 3)

        playback.toggle(KEY_A, blob, 16_000)
        player.positionMs = 7_000
        playback.tick()

        // What a reload produces: same bytes, different instance.
        val reloaded = blob.copyOf()
        assertFalse(reloaded === blob)
        val state = playback.stateFor(KEY_A, 16_000)

        assertTrue(state.isPlaying)
        assertEquals(7_000, state.positionMs)
        assertFalse(player.released)
        assertEquals(1, loader.loads)

        // And the message is still the same one after the reloaded array is
        // handed back to the bubble.
        playback.tick()
        assertTrue(playback.stateFor(KEY_A, 16_000).isPlaying)
        assertEquals(1, loader.loads)
    }

    @Test
    fun `tapping a second message stops the first`() {
        val first = FakePlayer()
        val second = FakePlayer(durationMs = 4_000)
        val loader = Loader(first, second)
        val playback = playback(loader)

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        playback.toggle(KEY_B, byteArrayOf(2), 4_000)

        assertTrue(first.released)
        assertFalse(second.released)
        assertFalse(playback.stateFor(KEY_A, 16_000).isPlaying)
        assertTrue(playback.stateFor(KEY_B, 4_000).isPlaying)
    }

    @Test
    fun `pause keeps the decoder so resuming picks up where it stopped`() {
        val player = FakePlayer()
        val loader = Loader(player)
        val focus = FakeFocus()
        val playback = playback(loader, focus)

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        player.positionMs = 5_500
        playback.tick()
        playback.toggle(KEY_A, byteArrayOf(1), 16_000)

        assertFalse(playback.stateFor(KEY_A, 16_000).isPlaying)
        assertEquals(5_500, playback.stateFor(KEY_A, 16_000).positionMs)
        assertFalse(player.released)
        assertFalse(focus.held)
        assertEquals(1, player.pauses)

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        assertTrue(playback.stateFor(KEY_A, 16_000).isPlaying)
        assertEquals(2, player.starts)
        assertEquals(1, loader.loads)
        assertTrue(focus.held)
    }

    @Test
    fun `losing audio focus pauses rather than dropping the message`() {
        val player = FakePlayer()
        val focus = FakeFocus()
        val playback = playback(Loader(player), focus)

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        focus.onLoss()

        assertFalse(playback.stateFor(KEY_A, 16_000).isPlaying)
        assertFalse(player.released)
    }

    @Test
    fun `a finished message hands its decoder back`() {
        val player = FakePlayer()
        val focus = FakeFocus()
        val playback = playback(Loader(player), focus)

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        player.onComplete()

        assertTrue(player.released)
        assertFalse(playback.stateFor(KEY_A, 16_000).isPlaying)
        assertNull(playback.activeKey)
        assertFalse(focus.held)
    }

    @Test
    fun `a blob that will not decode reports a failure and keeps its stated duration`() {
        val playback = playback(Loader(null))

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        val state = playback.stateFor(KEY_A, 16_000)

        assertTrue(state.display.failed)
        assertEquals(16_000, state.display.totalMs)
        assertFalse(state.isPlaying)
    }

    @Test
    fun `a decoder error mid-message surfaces the failure and releases the decoder`() {
        val player = FakePlayer()
        val playback = playback(Loader(player))

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        player.onError()

        assertTrue(player.released)
        assertTrue(playback.stateFor(KEY_A, 16_000).display.failed)
        assertFalse(playback.stateFor(KEY_B, 4_000).display.failed)
    }

    @Test
    fun `the decoder's own duration replaces the sender's stated one`() {
        val playback = playback(Loader(FakePlayer(durationMs = 16_450)))

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)

        assertEquals(16_450, playback.stateFor(KEY_A, 16_000).display.totalMs)
    }

    @Test
    fun `an untouched message reads as stopped at its stated duration`() {
        val playback = playback(Loader(FakePlayer()))

        val state = playback.stateFor(KEY_B, 4_000)

        assertFalse(state.isPlaying)
        assertEquals(0, state.positionMs)
        assertEquals(4_000, state.display.totalMs)
        assertFalse(state.display.failed)
    }

    @Test
    fun `leaving the screen releases the decoder`() {
        val player = FakePlayer()
        val focus = FakeFocus()
        val playback = playback(Loader(player), focus)

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        playback.release()

        assertTrue(player.released)
        assertFalse(focus.held)
        assertNull(playback.activeKey)
    }

    /**
     * A decode is slow enough to lose a race with the next tap. The loser's
     * decoder has to be released rather than left playing under the winner.
     */
    @Test
    fun `a decode that lands after a newer tap is thrown away`() {
        val slow = FakePlayer()
        val fast = FakePlayer(durationMs = 4_000)
        var deliverSlow: (() -> Unit)? = null
        var loads = 0
        val playback = VoiceMessagePlayback(
            focus = FakeFocus(),
            load = { _, onPrepared ->
                loads += 1
                if (loads == 1) deliverSlow = { onPrepared(slow) } else onPrepared(fast)
            },
        )

        playback.toggle(KEY_A, byteArrayOf(1), 16_000)
        playback.toggle(KEY_B, byteArrayOf(2), 4_000)
        deliverSlow!!()

        assertTrue(slow.released)
        assertFalse(fast.released)
        assertEquals(KEY_B, playback.activeKey)
        assertTrue(playback.stateFor(KEY_B, 4_000).isPlaying)
    }
}
