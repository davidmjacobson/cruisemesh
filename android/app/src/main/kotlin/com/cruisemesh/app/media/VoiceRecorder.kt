package com.cruisemesh.app.media

import android.content.Context
import android.media.MediaRecorder
import android.os.SystemClock
import android.util.Log
import uniffi.cruisemesh_core.CoreVoiceCapturePlan
import uniffi.cruisemesh_core.voiceCapturePlan
import java.io.File

private const val TAG = "VoiceRecorder"

/**
 * Short push-to-talk voice capture for attachment messages.
 *
 * Every number here comes from the core's [voiceCapturePlan] so Android and iOS
 * cannot drift apart on what fits in one envelope; see `core/src/voice.rs` for
 * the bitrate/duration arithmetic. This class only owns start/stop and the temp
 * file — the duration bound itself is enforced by the composer's gesture state
 * machine, also in core.
 *
 * AAC-LC in MPEG-4 is the container on both platforms. `MediaRecorder`'s
 * documented encoder set at minSdk 31 includes AAC (Opus arrived in API 29 but
 * only into an Ogg container, which iOS `AVAudioPlayer` cannot read), so this is
 * the one encoding both shells can produce *and* play.
 */
class VoiceRecorder(private val context: Context) {
    private var recorder: MediaRecorder? = null
    private var outputFile: File? = null
    private var startedAtMs: Long = 0L

    /**
     * Set when `MediaRecorder` hit [MAX_DURATION_BACKSTOP_MS] and stopped
     * itself. It has already finalized the MPEG-4 `moov` atom by then, so the
     * file on disk is complete and playable — but calling [MediaRecorder.stop]
     * on it a second time throws, and treating that throw as a failed recording
     * would delete the very file the backstop existed to save.
     */
    private var selfStopped = false

    val isRecording: Boolean get() = recorder != null

    /**
     * Bytes the encoder has written so far, for the composer's byte-budget
     * check. Zero when nothing is recording.
     *
     * This is an in-progress MPEG-4 file, so it understates the finished size by
     * the sample tables still to be written at stop; the core's byte budget
     * holds a container allowance back for exactly that.
     */
    fun bytesRecorded(): Long = if (recorder == null) 0L else outputFile?.length() ?: 0L

    fun start(): Boolean {
        stopInternal(deleteFile = true)
        val plan = voiceCapturePlan()
        val dir = File(context.cacheDir, "voice").apply { mkdirs() }
        val file = File(dir, "memo-${System.currentTimeMillis()}.m4a")
        return try {
            // minSdk is 31 (S), so the context-taking constructor always exists.
            val mediaRecorder = MediaRecorder(context)
            // Deliberately still MIC. The spec suggested VOICE_COMMUNICATION for
            // its gain control and noise suppression, but that source asks the
            // platform for a communication-tuned uplink, which on some devices
            // pulls capture onto a Bluetooth headset's HFP profile. Fighting the
            // mesh over the same radio is the one thing audio on this project is
            // not allowed to do, and none of it is verifiable off-device — so
            // this keeps the source that has been in the field.
            mediaRecorder.setAudioSource(MediaRecorder.AudioSource.MIC)
            mediaRecorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            mediaRecorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            mediaRecorder.setAudioEncodingBitRate(plan.bitrateBps.toInt())
            mediaRecorder.setAudioSamplingRate(plan.sampleRateHz.toInt())
            mediaRecorder.setAudioChannels(1)
            // Backstop only. The composer stops at the plan's bound; this fires
            // a few seconds later and covers a UI that somehow stopped ticking.
            mediaRecorder.setMaxDuration(plan.maxDurationMs.toInt() + MAX_DURATION_BACKSTOP_MS)
            mediaRecorder.setOnInfoListener { _, what, _ ->
                if (what == MediaRecorder.MEDIA_RECORDER_INFO_MAX_DURATION_REACHED) {
                    // The recording is finished and on disk; [stop] must read it
                    // rather than try to stop an already-stopped recorder.
                    selfStopped = true
                }
            }
            mediaRecorder.setOutputFile(file.absolutePath)
            mediaRecorder.prepare()
            mediaRecorder.start()
            recorder = mediaRecorder
            outputFile = file
            // Monotonic, not wall time: a carrier or NTP correction landing
            // mid-hold must not shorten or lengthen what the user recorded.
            startedAtMs = SystemClock.elapsedRealtime()
            true
        } catch (e: Exception) {
            Log.w(TAG, "Failed to start voice recorder: ${e.message}")
            stopInternal(deleteFile = true)
            false
        }
    }

    /**
     * Stops recording and returns the file + duration, or null if nothing
     * usable was captured.
     */
    fun stop(): Pair<File, Int>? {
        val file = outputFile
        val started = startedAtMs
        val mediaRecorder = recorder
        val alreadyFinalized = selfStopped
        recorder = null
        outputFile = null
        startedAtMs = 0L
        selfStopped = false
        if (mediaRecorder == null || file == null) return null
        var stopFailed = false
        if (!alreadyFinalized) {
            try {
                mediaRecorder.stop()
            } catch (e: Exception) {
                // Nothing recorded, or a state the recorder cannot stop from.
                Log.w(TAG, "Failed to stop voice recorder: ${e.message}")
                stopFailed = true
            }
        }
        try {
            mediaRecorder.release()
        } catch (_: Exception) {
        }
        val elapsed = (SystemClock.elapsedRealtime() - started).coerceAtLeast(0L)
        val durationMs = elapsed.coerceAtMost(voiceCapturePlan().maxDurationMs.toLong()).toInt()
        if (stopFailed || !file.exists() || file.length() == 0L) {
            file.delete()
            return null
        }
        return file to durationMs
    }

    fun cancel() {
        stopInternal(deleteFile = true)
    }

    private fun stopInternal(deleteFile: Boolean) {
        val mediaRecorder = recorder
        val file = outputFile
        val alreadyFinalized = selfStopped
        recorder = null
        outputFile = null
        startedAtMs = 0L
        selfStopped = false
        if (mediaRecorder != null) {
            if (!alreadyFinalized) {
                try {
                    mediaRecorder.stop()
                } catch (_: Exception) {
                }
            }
            try {
                mediaRecorder.release()
            } catch (_: Exception) {
            }
        }
        if (deleteFile) {
            file?.delete()
        }
    }

    companion object {
        /** Slack between the composer's own stop and the recorder's hard stop. */
        private const val MAX_DURATION_BACKSTOP_MS = 5_000

        /** Recorder configuration and gesture bounds, owned by the core. */
        val plan: CoreVoiceCapturePlan get() = voiceCapturePlan()
    }
}
