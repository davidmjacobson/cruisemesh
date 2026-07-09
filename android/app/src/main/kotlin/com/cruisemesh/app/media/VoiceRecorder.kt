package com.cruisemesh.app.media

import android.content.Context
import android.media.MediaRecorder
import android.os.Build
import android.util.Log
import java.io.File

private const val TAG = "VoiceRecorder"

/**
 * Short AAC voice memo capture for attachment messages. Max duration is
 * enforced by the caller; this class only owns start/stop and the temp file.
 */
class VoiceRecorder(private val context: Context) {
    private var recorder: MediaRecorder? = null
    private var outputFile: File? = null
    private var startedAtMs: Long = 0L

    val isRecording: Boolean get() = recorder != null

    fun start(): Boolean {
        stopInternal(deleteFile = true)
        val dir = File(context.cacheDir, "voice").apply { mkdirs() }
        val file = File(dir, "memo-${System.currentTimeMillis()}.m4a")
        return try {
            val mediaRecorder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                MediaRecorder(context)
            } else {
                @Suppress("DEPRECATION")
                MediaRecorder()
            }
            mediaRecorder.setAudioSource(MediaRecorder.AudioSource.MIC)
            mediaRecorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            mediaRecorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            mediaRecorder.setAudioEncodingBitRate(32_000)
            mediaRecorder.setAudioSamplingRate(16_000)
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
            val durationMs = (System.currentTimeMillis() - started).toInt().coerceAtLeast(0)
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
}
