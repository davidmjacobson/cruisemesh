package com.cruisemesh.app.chat

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The composer's data-loss invariant: a send that did not reach the durable
 * local transaction leaves every character the user typed exactly where it was.
 *
 * Field report this pins: type a message, tap send, the send fails, the text
 * vanishes with nothing stored, nothing queued and nothing said. Both chat
 * screens used to clear the field on the way past the send call, so the only
 * thing standing between a failed send and lost words was that the call
 * happened not to fail. [ComposerSendPolicy] is now the sole place that can
 * empty the composer, and these tests are what hold it to that.
 */
class ComposerSendPolicyTest {

    private val photo = byteArrayOf(1, 2, 3)

    @Test
    fun `a failed text send keeps the draft intact and reports the failure`() {
        var sentText: String? = null

        val outcome = ComposerSendPolicy.attempt(
            draft = "meet you on the pool deck",
            pendingPhoto = null,
            sendPhoto = { _, _ -> error("no photo staged") },
            sendText = { text ->
                sentText = text
                SendResult.FAILED
            },
        )

        assertEquals("meet you on the pool deck", sentText)
        assertEquals("meet you on the pool deck", outcome.draft)
        assertNull(outcome.pendingPhoto)
        assertEquals(ComposerSendStatus.NOT_QUEUED, outcome.status)
    }

    @Test
    fun `a stored text send clears the draft`() {
        val outcome = ComposerSendPolicy.attempt(
            draft = "meet you on the pool deck",
            pendingPhoto = null,
            sendPhoto = { _, _ -> error("no photo staged") },
            sendText = { SendResult.STORED },
        )

        assertEquals("", outcome.draft)
        assertNull(outcome.pendingPhoto)
        assertEquals(ComposerSendStatus.QUEUED, outcome.status)
    }

    @Test
    fun `a failed send hands back the untrimmed draft so retrying costs one tap`() {
        // What goes on the wire is trimmed; what stays in the field is not, so
        // the cursor and any trailing newline survive a failure untouched.
        val typed = "  still here\n"

        val outcome = ComposerSendPolicy.attempt(
            draft = typed,
            pendingPhoto = null,
            sendPhoto = { _, _ -> error("no photo staged") },
            sendText = { text ->
                assertEquals("still here", text)
                SendResult.FAILED
            },
        )

        assertEquals(typed, outcome.draft)
    }

    @Test
    fun `a failed photo send keeps both the staged photo and its caption`() {
        var sentCaption: String? = null

        val outcome = ComposerSendPolicy.attempt(
            draft = "  from the top deck  ",
            pendingPhoto = photo,
            sendPhoto = { staged, caption ->
                assertArrayEquals(photo, staged)
                sentCaption = caption
                SendResult.FAILED
            },
            sendText = { error("a staged photo must win over bare text") },
        )

        assertEquals("from the top deck", sentCaption)
        assertEquals("  from the top deck  ", outcome.draft)
        assertArrayEquals(photo, outcome.pendingPhoto)
        assertEquals(ComposerSendStatus.NOT_QUEUED, outcome.status)
    }

    @Test
    fun `a stored photo send clears both the staged photo and the caption`() {
        val outcome = ComposerSendPolicy.attempt(
            draft = "from the top deck",
            pendingPhoto = photo,
            sendPhoto = { _, _ -> SendResult.STORED },
            sendText = { error("a staged photo must win over bare text") },
        )

        assertEquals("", outcome.draft)
        assertNull(outcome.pendingPhoto)
        assertEquals(ComposerSendStatus.QUEUED, outcome.status)
    }

    @Test
    fun `an empty composer attempts nothing and is not reported as a failure`() {
        var attempts = 0

        val outcome = ComposerSendPolicy.attempt(
            draft = "   \n ",
            pendingPhoto = null,
            sendPhoto = { _, _ ->
                attempts++
                SendResult.STORED
            },
            sendText = { _ ->
                attempts++
                SendResult.STORED
            },
        )

        assertEquals(0, attempts)
        assertEquals("   \n ", outcome.draft)
        assertEquals(ComposerSendStatus.NOTHING_TO_SEND, outcome.status)
    }

    @Test
    fun `a draft that failed once survives every retry until one is stored`() {
        var draft = "keep me"
        var attempt = 0

        repeat(3) {
            val outcome = ComposerSendPolicy.attempt(
                draft = draft,
                pendingPhoto = null,
                sendPhoto = { _, _ -> error("no photo staged") },
                sendText = {
                    attempt++
                    if (attempt < 3) SendResult.FAILED else SendResult.STORED
                },
            )
            draft = outcome.draft
        }

        assertEquals(3, attempt)
        assertTrue("the composer must not empty until a send is stored", draft.isEmpty())
    }
}
