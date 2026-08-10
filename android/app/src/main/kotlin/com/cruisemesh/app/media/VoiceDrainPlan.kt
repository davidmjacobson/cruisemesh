package com.cruisemesh.app.media

/**
 * The tail-drain decision for voice capture, kept pure (no Android imports) so it
 * is unit-tested directly on the JVM without touching audio hardware.
 *
 * Field symptom this exists for: holding the mic button, speaking, and lifting
 * the finger on the last word dropped roughly the last 0.4-0.5 s of audio — the
 * word spoken as the finger came off was not in the recording. The gesture fires
 * on time; the loss is that `MediaRecorder.stop()` tears the capture + AAC
 * encoder pipeline down immediately, discarding the in-flight buffered audio that
 * had not been encoded yet. Keeping the recorder running for a short window after
 * release lets that tail get encoded before finalize.
 */
internal object VoiceDrainPlan {
    /**
     * How long to keep the recorder running after the user releases, before
     * `MediaRecorder.stop()` finalizes the file.
     *
     * The AAC-LC encoder frame is 1024 samples, and the
     * capture -> AudioFlinger -> MediaCodec pipeline buffers many such frames on
     * top of the `AudioRecord` ring buffer. `stop()` discards all of that. The
     * field-measured loss on two phones was ~0.4-0.5 s, which is an order of
     * magnitude larger than the `AudioRecord` minimum buffer alone (tens of ms;
     * see [minBufferLatencyMs]) — the encoder/AudioFlinger buffering dominates
     * and is not queryable through `MediaRecorder`, so a fixed window is the
     * honest bound rather than a derived one. 500 ms covers the observed loss
     * with a little margin; the cost is a small amount of trailing ambient audio,
     * which is the correct trade against clipping the last spoken word.
     */
    const val DRAIN_WINDOW_MS: Long = 500L

    /**
     * The `AudioRecord` latency implied by a minimum buffer of [minBufferBytes]
     * at [sampleRateHz] (mono, 16-bit PCM), in milliseconds.
     *
     * This is deliberately *not* what sizes the drain: it exists to show that the
     * min-buffer contribution is an order of magnitude below [DRAIN_WINDOW_MS],
     * which is why the fixed window — not a value derived from
     * `AudioRecord.getMinBufferSize` — is what ships.
     */
    fun minBufferLatencyMs(minBufferBytes: Int, sampleRateHz: Int): Long {
        if (sampleRateHz <= 0 || minBufferBytes <= 0) return 0L
        // 16-bit mono PCM is 2 bytes per frame.
        val frames = (minBufferBytes / 2).toLong()
        return frames * 1000L / sampleRateHz
    }

    /**
     * The window to keep recording after release before calling `stop()`.
     *
     * @param alreadyFinalized `true` when the `MediaRecorder` self-stopped at its
     *   max-duration backstop (`selfStopped` /
     *   `MEDIA_RECORDER_INFO_MAX_DURATION_REACHED`): the `moov` atom is already
     *   written and `stop()` must not run again, so there is nothing to drain.
     * @return `0` to stop immediately (a backstop finalize, or a cancel), else
     *   [DRAIN_WINDOW_MS] for a normal release/send.
     */
    fun drainWindowMs(alreadyFinalized: Boolean): Long =
        if (alreadyFinalized) 0L else DRAIN_WINDOW_MS
}
