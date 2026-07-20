package com.cruisemesh.app.chat

/**
 * Pure math for the swipe-to-reply gesture (T1), kept Android-free so it can be
 * unit-tested directly. The composable in [ChatScreen]/[GroupChatScreen] owns
 * the animation and focus; this only decides how far the bubble follows the
 * finger and whether a release starts a reply.
 */
object SwipeToReplyLogic {
    /** Past [maxDrag] the bubble keeps moving but at a fraction of the finger, so it feels resistant. */
    const val RUBBER_BAND = 0.15f

    /**
     * The horizontal offset the bubble should show for a raw rightward drag of
     * [rawDrag] px. Leftward drags are ignored (return 0); the reply gesture is
     * a right swipe only. Past [maxDrag] the offset rubber-bands.
     */
    fun clampOffset(rawDrag: Float, maxDrag: Float): Float =
        when {
            rawDrag <= 0f -> 0f
            rawDrag <= maxDrag -> rawDrag
            else -> maxDrag + (rawDrag - maxDrag) * RUBBER_BAND
        }

    /** Whether releasing at [offset] (px) should start a reply. */
    fun shouldReply(offset: Float, threshold: Float): Boolean = offset >= threshold

    /** Fraction 0..1 of the way to the trigger threshold, for icon fade/scale. */
    fun progress(offset: Float, threshold: Float): Float =
        if (threshold <= 0f) 0f else (offset / threshold).coerceIn(0f, 1f)
}
