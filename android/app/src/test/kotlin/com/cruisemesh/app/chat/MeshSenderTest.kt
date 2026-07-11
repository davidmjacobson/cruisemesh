package com.cruisemesh.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

class MeshSenderTest {

    @Test
    fun `normal path -- own history is highest, so it wins`() {
        // No receipts ever recorded (fresh chat, or peer hasn't acked yet):
        // ordinary +1 off our own contiguous history.
        assertEquals(
            6uL,
            nextAuthoredLamport(ownContiguous = 5uL, ackedDelivered = 0uL, ackedRead = 0uL),
        )
    }

    @Test
    fun `wiped history -- read watermark wins`() {
        // Chat was deleted and the contact re-added: our own history reset
        // to 0, but the peer replayed a persisted "read through 5" receipt
        // from before the wipe. Must not reissue lamports 1..5, which the
        // peer still holds and would silently dedup-drop while rendering as
        // read.
        assertEquals(
            6uL,
            nextAuthoredLamport(ownContiguous = 0uL, ackedDelivered = 0uL, ackedRead = 5uL),
        )
    }

    @Test
    fun `delivered watermark beats a lower read watermark`() {
        assertEquals(
            9uL,
            nextAuthoredLamport(ownContiguous = 2uL, ackedDelivered = 8uL, ackedRead = 3uL),
        )
    }

    @Test
    fun `all zero -- first message ever gets lamport 1`() {
        assertEquals(
            1uL,
            nextAuthoredLamport(ownContiguous = 0uL, ackedDelivered = 0uL, ackedRead = 0uL),
        )
    }
}
