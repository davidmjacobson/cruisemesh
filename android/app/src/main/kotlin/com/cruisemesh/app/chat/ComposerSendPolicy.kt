package com.cruisemesh.app.chat

/** What one tap of send did, and therefore what the screen owes the user. */
enum class ComposerSendStatus {
    /** An empty composer: nothing was attempted and nothing changed. */
    NOTHING_TO_SEND,

    /** The message reached the durable local message/outbound transaction. */
    QUEUED,

    /** It did not. Every character the user typed is still in the composer. */
    NOT_QUEUED,
}

/**
 * The composer's contents *after* a send attempt, plus [status].
 *
 * The screen assigns [draft] and [pendingPhoto] back unconditionally, so the
 * only code that can ever empty the composer is [ComposerSendPolicy.attempt]
 * -- and it only does so on [ComposerSendStatus.QUEUED].
 */
class ComposerSendOutcome internal constructor(
    val draft: String,
    val pendingPhoto: ByteArray?,
    val status: ComposerSendStatus,
)

/**
 * The one rule the composer follows when send is tapped: the typed text
 * survives unless the message was durably queued.
 *
 * This used to live inline in [ChatScreen] and [GroupChatScreen] as
 * `send(...); draft = ""`, which cleared the field on the way past a send that
 * had failed -- the user watched their message disappear with nothing stored,
 * nothing queued and nothing said. Extracted here as a plain class with no
 * Android imports so that rule is unit-testable rather than reachable only
 * through a live Compose tree and a real mesh transport.
 *
 * Failure is preserved verbatim, not re-trimmed: the draft handed back is the
 * exact string the user had, trailing newline and all, so retrying costs a tap
 * rather than retyping.
 */
object ComposerSendPolicy {
    /**
     * Attempts one send of the current composer contents.
     *
     * A staged photo wins over bare text -- the trimmed [draft] rides along as
     * its caption, which is how a single tap sends "photo + words" as one
     * attachment rather than two messages.
     */
    fun attempt(
        draft: String,
        pendingPhoto: ByteArray?,
        sendPhoto: (photo: ByteArray, caption: String) -> SendResult,
        sendText: (text: String) -> SendResult,
    ): ComposerSendOutcome {
        val text = draft.trim()
        val result = when {
            pendingPhoto != null -> sendPhoto(pendingPhoto, text)
            text.isNotEmpty() -> sendText(text)
            else -> return ComposerSendOutcome(draft, pendingPhoto, ComposerSendStatus.NOTHING_TO_SEND)
        }
        return when (result) {
            SendResult.STORED -> ComposerSendOutcome("", null, ComposerSendStatus.QUEUED)
            SendResult.FAILED -> ComposerSendOutcome(draft, pendingPhoto, ComposerSendStatus.NOT_QUEUED)
        }
    }
}
