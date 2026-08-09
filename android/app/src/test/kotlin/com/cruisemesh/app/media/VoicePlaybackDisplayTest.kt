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
}
