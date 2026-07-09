package com.cruisemesh.app.media

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AttachmentPayloadTest {

    @Test
    fun `round-trips image attachment with caption`() {
        val original = AttachmentPayload(
            mediaType = AttachmentPayload.MediaType.IMAGE,
            mimeType = "image/jpeg",
            durationMs = 0,
            blob = byteArrayOf(1, 2, 3, 4, 5),
            caption = "pool deck",
        )
        val decoded = AttachmentPayload.decode(original.encode())
        assertNotNull(decoded)
        assertEquals(original, decoded)
    }

    @Test
    fun `round-trips voice memo without caption`() {
        val original = AttachmentPayload(
            mediaType = AttachmentPayload.MediaType.AUDIO,
            mimeType = "audio/mp4",
            durationMs = 12_345,
            blob = ByteArray(64) { it.toByte() },
        )
        val decoded = AttachmentPayload.decode(original.encode())
        assertNotNull(decoded)
        assertEquals(AttachmentPayload.MediaType.AUDIO, decoded!!.mediaType)
        assertEquals(12_345, decoded.durationMs)
        assertArrayEquals(original.blob, decoded.blob)
        assertEquals("", decoded.caption)
    }

    @Test
    fun `rejects unknown version`() {
        val good = AttachmentPayload(
            mediaType = AttachmentPayload.MediaType.IMAGE,
            mimeType = "image/jpeg",
            durationMs = 0,
            blob = byteArrayOf(9),
        ).encode()
        good[0] = 99
        assertNull(AttachmentPayload.decode(good))
    }

    @Test
    fun `preview labels cover photo and voice`() {
        val photo = AttachmentPayload(
            mediaType = AttachmentPayload.MediaType.IMAGE,
            mimeType = "image/jpeg",
            durationMs = 0,
            blob = byteArrayOf(1),
        )
        val voice = AttachmentPayload(
            mediaType = AttachmentPayload.MediaType.AUDIO,
            mimeType = "audio/mp4",
            durationMs = 4_200,
            blob = byteArrayOf(1),
        )
        assertEquals("📷 Photo", AttachmentPayload.previewLabel(photo))
        assertTrue(AttachmentPayload.previewLabel(voice).startsWith("🎤 Voice memo"))
        assertEquals(
            "📷 Photo",
            AttachmentPayload.previewLabelFromRaw(KIND_ATTACHMENT_MANIFEST, photo.encode()),
        )
        assertNull(AttachmentPayload.previewLabelFromRaw(1u, photo.encode()))
    }

    @Test
    fun `visible chat kinds include text and attachments only`() {
        assertTrue(isVisibleChatKind(1u))
        assertTrue(isVisibleChatKind(KIND_ATTACHMENT_MANIFEST))
        assertTrue(!isVisibleChatKind(3u))
        assertTrue(!isVisibleChatKind(2u))
    }
}
