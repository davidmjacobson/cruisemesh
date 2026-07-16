package com.cruisemesh.app.chat

import com.cruisemesh.app.media.AttachmentPayload
import com.cruisemesh.app.media.KIND_ATTACHMENT_MANIFEST
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.cruisemesh_core.StoredMessage

class ReplyPresentationTest {
    @Test
    fun `missing quote target has a stable fallback`() {
        val preview = quotedMessagePreview(null) { "unused" }

        assertEquals("Original message unavailable", preview.text)
        assertNull(preview.senderLabel)
        assertNull(preview.target)
    }

    @Test
    fun `photo quote uses attachment preview instead of binary content`() {
        val photo = AttachmentPayload(
            mediaType = AttachmentPayload.MediaType.IMAGE,
            mimeType = "image/jpeg",
            durationMs = 0,
            blob = byteArrayOf(1, 2, 3),
            caption = "pool deck",
        )
        val message = StoredMessage(
            chatId = byteArrayOf(1),
            senderUserId = byteArrayOf(2),
            lamport = 1uL,
            timestamp = 1L,
            kind = KIND_ATTACHMENT_MANIFEST,
            payload = photo.encode(),
        )

        assertEquals("📷 pool deck", quotedMessageText(message))
    }
}
