package com.cruisemesh.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VoicePlaybackDisplayTest {
    @Test
    fun `initial shows the manifest duration and no failure`() {
        val display = VoicePlaybackDisplay.initial(12_000)
        assertEquals(12_000, display.totalMs)
        assertFalse(display.failed)
    }

    @Test
    fun `a negative manifest duration is floored at zero`() {
        assertEquals(0, VoicePlaybackDisplay.initial(-5).totalMs)
    }

    @Test
    fun `a decode failure surfaces the error but keeps the manifest duration`() {
        val display = VoicePlaybackDisplay.initial(9_000).withFailure()
        assertTrue(display.failed)
        assertEquals(9_000, display.totalMs)
    }

    @Test
    fun `a positive decoder duration replaces the manifest duration`() {
        val display = VoicePlaybackDisplay.initial(9_000).withDecoderDuration(9_450)
        assertEquals(9_450, display.totalMs)
        assertFalse(display.failed)
    }

    @Test
    fun `a zero or unknown decoder duration keeps the manifest duration`() {
        val fromZero = VoicePlaybackDisplay.initial(9_000).withDecoderDuration(0)
        assertEquals(9_000, fromZero.totalMs)

        val fromUnknown = VoicePlaybackDisplay.initial(9_000).withDecoderDuration(-1)
        assertEquals(9_000, fromUnknown.totalMs)
    }

    @Test
    fun `retrying clears a prior failure without zeroing the duration`() {
        val display = VoicePlaybackDisplay.initial(9_000).withFailure().retrying()
        assertFalse(display.failed)
        assertEquals(9_000, display.totalMs)
    }

    @Test
    fun `recovering after a failure never blanks the length to zero`() {
        // The whole point: a failed decode must not leave the bubble reading
        // 0:00 -- the manifest duration is retained throughout.
        val afterFailure = VoicePlaybackDisplay.initial(15_000).withFailure()
        assertEquals(15_000, afterFailure.totalMs)
        val afterRetry = afterFailure.retrying()
        assertEquals(15_000, afterRetry.totalMs)
    }

    @Test
    fun `a bar showing only the sender duration cannot be seeked`() {
        val display = VoicePlaybackDisplay.initial(16_000)
        assertFalse(display.canSeek)
        assertEquals(null, display.seekTargetMs(0.5f))
        assertEquals(null, VoicePlaybackDisplay.seekTargetMs(null, 0.5f))
        assertEquals(null, VoicePlaybackDisplay.seekTargetMs(0, 0.5f))
        assertEquals(null, VoicePlaybackDisplay.seekTargetMs(-1, 0.5f))
    }

    @Test
    fun `a decoder duration turns a bar fraction into a clamped millisecond target`() {
        val display = VoicePlaybackDisplay.initial(9_000).withDecoderDuration(10_000)
        assertTrue(display.canSeek)
        assertEquals(0, display.seekTargetMs(0f))
        assertEquals(5_000, display.seekTargetMs(0.5f))
        assertEquals(10_000, display.seekTargetMs(1f))
        assertEquals(0, display.seekTargetMs(-2f))
        assertEquals(10_000, display.seekTargetMs(3f))
        assertEquals(null, display.seekTargetMs(Float.NaN))
    }

    @Test
    fun `progress is zero when the total is not a length`() {
        assertEquals(0f, VoicePlaybackDisplay.progressFraction(4_000, 0), 0f)
        assertEquals(0.25f, VoicePlaybackDisplay.progressFraction(4_000, 16_000), 0f)
        assertEquals(0f, VoicePlaybackDisplay.progressFraction(-1, 16_000), 0f)
        assertEquals(1f, VoicePlaybackDisplay.progressFraction(20_000, 16_000), 0f)
    }
}
