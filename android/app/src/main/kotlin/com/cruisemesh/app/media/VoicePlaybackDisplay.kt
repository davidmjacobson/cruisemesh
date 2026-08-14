package com.cruisemesh.app.media

/**
 * Pure display state for a received voice-message bubble's "elapsed / total"
 * line and its error affordance. No Android imports, so it unit-tests directly
 * (CLAUDE.md: schedule/policy logic is a plain class); the bubble owns the
 * MediaPlayer, this only decides what the row shows.
 *
 * The invariant it enforces: a decode/prepare/playback failure must NEVER blank
 * the total out to 0:00. The bubble keeps the sender's manifest duration and
 * raises a "couldn't play" state instead, so a bad decode on the receiver reads
 * as a playback problem, not a zero-length message — matching the iOS bubble,
 * which keeps its stated duration and shows the same caption.
 */
data class VoicePlaybackDisplay(
    val totalMs: Int,
    val failed: Boolean,
    /**
     * What the decoder reported, if it has reported a positive length.
     * The bar may still show [totalMs] from the sender's manifest before
     * that; that number is not a safe seek target.
     */
    val decoderDurationMs: Int? = null,
) {
    /** A scrub is only legal once the decoder has named a real length. */
    val canSeek: Boolean get() = (decoderDurationMs ?: 0) > 0

    /**
     * The decoder reported its own duration once the file prepared. Trust it
     * only when positive: a file that prepared but reports 0/unknown keeps the
     * manifest duration rather than collapsing the bar to nothing.
     */
    fun withDecoderDuration(decoderDurationMs: Int): VoicePlaybackDisplay =
        if (decoderDurationMs > 0) {
            copy(
                totalMs = decoderDurationMs,
                failed = false,
                decoderDurationMs = decoderDurationMs,
            )
        } else {
            copy(failed = false)
        }

    /**
     * Map a 0…1 bar position onto milliseconds. Null when a seek would be
     * guessing: no decoder duration yet, or a non-finite fraction.
     */
    fun seekTargetMs(fraction: Float): Int? =
        Companion.seekTargetMs(decoderDurationMs, fraction)

    /** A prepare/decode/playback failure: surface it, keep the manifest total. */
    fun withFailure(): VoicePlaybackDisplay = copy(failed = true)

    /** A fresh attempt is starting; clear any prior failure but keep the total. */
    fun retrying(): VoicePlaybackDisplay = copy(failed = false)

    companion object {
        fun initial(manifestDurationMs: Int): VoicePlaybackDisplay =
            VoicePlaybackDisplay(totalMs = manifestDurationMs.coerceAtLeast(0), failed = false)

        /**
         * Map a 0…1 bar position onto milliseconds of decoder time.
         * Null when [decoderDurationMs] is missing or not positive, or when
         * [fraction] is not a real number — never treat the sender's stated
         * duration as a seek target.
         */
        fun seekTargetMs(decoderDurationMs: Int?, fraction: Float): Int? {
            val duration = decoderDurationMs ?: return null
            if (duration <= 0 || !fraction.isFinite()) return null
            val clamped = fraction.coerceIn(0f, 1f)
            return (clamped * duration).toInt().coerceIn(0, duration)
        }

        fun progressFraction(positionMs: Int, totalMs: Int): Float {
            if (totalMs <= 0) return 0f
            return (positionMs.toFloat() / totalMs).coerceIn(0f, 1f)
        }
    }
}
