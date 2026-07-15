package com.cruisemesh.app.chat

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.IntSize
import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.StoredMessage

class PhotoViewerTest {
    @Test
    fun `image bytes are extracted but audio and text are ignored`() {
        val jpeg = byteArrayOf(1, 2, 3)
        val photo = stored(
            KIND_ATTACHMENT_MANIFEST,
            AttachmentPayload(AttachmentPayload.MediaType.IMAGE, "image/jpeg", 0, jpeg).encode(),
        )
        val audio = stored(
            KIND_ATTACHMENT_MANIFEST,
            AttachmentPayload(AttachmentPayload.MediaType.AUDIO, "audio/mp4", 1000, byteArrayOf(4)).encode(),
        )

        assertArrayEquals(jpeg, messageImageBytes(photo))
        assertNull(messageImageBytes(audio))
        assertNull(messageImageBytes(stored(1u, "hello".toByteArray())))
    }

    @Test
    fun `zoom and pan stay inside viewer bounds`() {
        assertEquals(5f, clampedPhotoScale(4f, 2f))
        assertEquals(1f, clampedPhotoScale(1f, 0.5f))
        assertEquals(
            Offset(100f, -200f),
            clampedPhotoOffset(Offset(500f, -500f), 2f, IntSize(200, 400)),
        )
        assertEquals(Offset.Zero, clampedPhotoOffset(Offset(10f, 10f), 1f, IntSize(200, 400)))
    }

    @Test
    fun `swipe down dismisses only at base zoom after threshold`() {
        assertTrue(shouldDismissPhotoViewer(scale = 1f, verticalDrag = 121f, threshold = 120f))
        assertFalse(shouldDismissPhotoViewer(scale = 1f, verticalDrag = 119f, threshold = 120f))
        assertFalse(shouldDismissPhotoViewer(scale = 2f, verticalDrag = 300f, threshold = 120f))
    }

    private fun stored(kind: UByte, payload: ByteArray) = StoredMessage(
        chatId = byteArrayOf(1),
        senderUserId = byteArrayOf(2),
        lamport = 1uL,
        timestamp = 1L,
        kind = kind,
        payload = payload,
    )
}
