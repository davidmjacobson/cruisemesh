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
) {
    /**
     * The decoder reported its own duration once the file prepared. Trust it
     * only when positive: a file that prepared but reports 0/unknown keeps the
     * manifest duration rather than collapsing the bar to nothing.
     */
    fun withDecoderDuration(decoderDurationMs: Int): VoicePlaybackDisplay =
        if (decoderDurationMs > 0) {
            copy(totalMs = decoderDurationMs, failed = false)
        } else {
            copy(failed = false)
        }

    /** A prepare/decode/playback failure: surface it, keep the manifest total. */
    fun withFailure(): VoicePlaybackDisplay = copy(failed = true)

    /** A fresh attempt is starting; clear any prior failure but keep the total. */
    fun retrying(): VoicePlaybackDisplay = copy(failed = false)

    companion object {
        fun initial(manifestDurationMs: Int): VoicePlaybackDisplay =
            VoicePlaybackDisplay(totalMs = manifestDurationMs.coerceAtLeast(0), failed = false)
    }
}
