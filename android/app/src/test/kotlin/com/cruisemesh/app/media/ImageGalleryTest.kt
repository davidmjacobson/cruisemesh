package com.cruisemesh.app.media

import org.junit.Assert.assertEquals
import org.junit.Test

class ImageGalleryTest {
    @Test
    fun fitSize_landscapeFitsWidth() {
        val (w, h) = ImageGallery.fitSize(
            sourceWidth = 1600,
            sourceHeight = 900,
            maxWidth = 300f,
            maxHeight = 360f,
        )
        assertEquals(300f, w, 0.01f)
        assertEquals(168.75f, h, 0.01f)
    }

    @Test
    fun fitSize_portraitFitsHeight() {
        val (w, h) = ImageGallery.fitSize(
            sourceWidth = 900,
            sourceHeight = 1600,
            maxWidth = 300f,
            maxHeight = 360f,
        )
        assertEquals(202.5f, w, 0.01f)
        assertEquals(360f, h, 0.01f)
    }

    @Test
    fun fitSize_squareUsesWidthWhenItFits() {
        val (w, h) = ImageGallery.fitSize(
            sourceWidth = 1000,
            sourceHeight = 1000,
            maxWidth = 300f,
            maxHeight = 360f,
        )
        assertEquals(300f, w)
        assertEquals(300f, h)
    }

    @Test
    fun fitSize_rejectsInvalidInputs() {
        assertEquals(0f to 0f, ImageGallery.fitSize(0, 100, 300f, 360f))
        assertEquals(0f to 0f, ImageGallery.fitSize(100, 100, 0f, 360f))
    }
}
