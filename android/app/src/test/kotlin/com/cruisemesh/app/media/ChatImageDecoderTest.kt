package com.cruisemesh.app.media

import org.junit.Assert.assertEquals
import org.junit.Test

class ChatImageDecoderTest {
    @Test
    fun sampleSizeFor_returnsOneWhenAlreadySmallerThanTarget() {
        assertEquals(1, ChatImageDecoder.sampleSizeFor(200, 150, 300, 360))
    }

    @Test
    fun sampleSizeFor_decodedSizeIsWithinTwoXTarget() {
        // 1024x768 source, bubble target ~300x360: sampleSize=2 -> 512x384,
        // which is <=2x the 300x360 target on both axes.
        val sample = ChatImageDecoder.sampleSizeFor(1024, 768, 300, 360)
        assertEquals(2, sample)
        val decodedWidth = 1024 / sample
        val decodedHeight = 768 / sample
        assertEquals(true, decodedWidth <= 300 * 2)
        assertEquals(true, decodedHeight <= 360 * 2)
    }

    @Test
    fun sampleSizeFor_largeSourceScalesDownFurther() {
        // 4032x3024 (typical phone-camera JPEG) vs a 300x360 bubble target.
        val sample = ChatImageDecoder.sampleSizeFor(4032, 3024, 300, 360)
        val decodedWidth = 4032 / sample
        val decodedHeight = 3024 / sample
        assertEquals(true, decodedWidth <= 300 * 2)
        assertEquals(true, decodedHeight <= 360 * 2)
        // And still a power of two.
        assertEquals(0, sample and (sample - 1))
    }

    @Test
    fun sampleSizeFor_rejectsInvalidInputs() {
        assertEquals(1, ChatImageDecoder.sampleSizeFor(0, 100, 300, 360))
        assertEquals(1, ChatImageDecoder.sampleSizeFor(100, 100, 0, 360))
    }
}
