package com.cruisemesh.app.media

import android.content.Context
import android.media.MediaRecorder
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

    val isRecording: Boolean get() = recorder != null

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
            mediaRecorder.setOutputFile(file.absolutePath)
            mediaRecorder.prepare()
            mediaRecorder.start()
            recorder = mediaRecorder
            outputFile = file
            startedAtMs = System.currentTimeMillis()
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
        recorder = null
        outputFile = null
        startedAtMs = 0L
        if (mediaRecorder == null || file == null) return null
        return try {
            mediaRecorder.stop()
            mediaRecorder.release()
            val elapsed = (System.currentTimeMillis() - started).coerceAtLeast(0L)
            val durationMs = elapsed.coerceAtMost(voiceCapturePlan().maxDurationMs.toLong()).toInt()
            if (!file.exists() || file.length() == 0L) {
                file.delete()
                null
            } else {
                file to durationMs
            }
        } catch (e: Exception) {
            Log.w(TAG, "Failed to stop voice recorder: ${e.message}")
            try {
                mediaRecorder.release()
            } catch (_: Exception) {
            }
            file.delete()
            null
        }
    }

    fun cancel() {
        stopInternal(deleteFile = true)
    }

    private fun stopInternal(deleteFile: Boolean) {
        val mediaRecorder = recorder
        val file = outputFile
        recorder = null
        outputFile = null
        startedAtMs = 0L
        if (mediaRecorder != null) {
            try {
                mediaRecorder.stop()
            } catch (_: Exception) {
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
