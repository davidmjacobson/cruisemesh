package com.cruisemesh.app.debug

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.os.Build
import android.os.Process
import android.util.Log
import androidx.core.content.FileProvider
import androidx.core.content.pm.PackageInfoCompat
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.concurrent.thread

/**
 * On-device persistent logging: streams this process's own logcat output to a
 * file under the app's external files dir so it can be retrieved and shared
 * without adb. This exists because the mesh's interesting failures happen while
 * the phone is out and about (left Wi-Fi, Bluetooth toggled, backgrounded) —
 * precisely when a laptop with adb isn't attached — so the log has to be
 * captured on-device and handed off via the share sheet (email/Drive/chat).
 *
 * Own-process only: since Android 4.1 an app's `logcat` can read just its own
 * UID's logs, and we additionally pin `--pid` to this process, so no other
 * app's data is captured. The app never logs message content — only metadata
 * (kinds, counts, addresses, delivery events) — so the capture is safe to
 * offer outside debug builds too. Debuggable builds capture always; release
 * builds capture only when explicitly enabled by an internal diagnostic
 * control ([setOptIn], persisted so it survives restarts).
 */
object DebugFileLog {
    private const val TAG = "DebugFileLog"
    private const val LOG_DIR = "logs"
    private const val FILE_NAME = "cruisemesh-log.txt"
    private const val ROTATED_NAME = "cruisemesh-log.1.txt"
    private const val PREFS_NAME = "debug_file_log"
    private const val KEY_OPT_IN = "opt_in"

    /** Rotate the active file once it passes this size; keep one older copy. */
    private const val MAX_BYTES = 4L * 1024 * 1024

    @Volatile private var started = false
    @Volatile private var logcatProcess: java.lang.Process? = null

    /**
     * Whether capture should run. Pure so the gate is unit-testable: always in
     * debuggable builds, opt-in otherwise.
     */
    fun shouldCapture(debuggable: Boolean, optedIn: Boolean): Boolean = debuggable || optedIn

    fun isDebuggableBuild(context: Context): Boolean =
        (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0

    fun isOptedIn(context: Context): Boolean =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(KEY_OPT_IN, false)

    fun isEnabled(context: Context): Boolean =
        shouldCapture(isDebuggableBuild(context), isOptedIn(context))

    /**
     * Release-build diagnostic opt-in. Enabling starts capture
     * immediately; disabling stops it (unless this is a debuggable build,
     * where capture is unconditional). The already-captured file is kept so
     * it can still be shared after turning the switch off.
     */
    fun setOptIn(context: Context, enabled: Boolean) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit().putBoolean(KEY_OPT_IN, enabled).apply()
        if (enabled) {
            start(context)
        } else if (!isDebuggableBuild(context)) {
            stopCapture()
        }
    }

    private fun logDir(context: Context): File =
        File(context.getExternalFilesDir(null), LOG_DIR).apply { mkdirs() }

    fun logFile(context: Context): File = File(logDir(context), FILE_NAME)

    /** Whether any captured log exists to share or delete. */
    fun hasCapturedLogs(context: Context): Boolean =
        logFile(context).let { it.exists() && it.length() > 0 } ||
            File(logDir(context), ROTATED_NAME).exists()

    /**
     * Erases every captured log.
     *
     * Capture keeps diagnostics on disk indefinitely once it has run, and the
     * entries include contact and group *names*, so a tester who turns the
     * switch off has to be able to erase what was already written -- not just
     * stop adding to it. Stops the capture thread first so nothing is holding
     * the file open, then restarts capture if it is still meant to be running
     * (an opted-in release build, or any debuggable build, where capture is
     * unconditional). Returns whether the files are gone afterwards.
     */
    @Synchronized
    fun deleteCapturedLogs(context: Context): Boolean {
        val wasCapturing = started
        stopCapture()
        val deleted = listOf(logFile(context), File(logDir(context), ROTATED_NAME))
            .all { !it.exists() || it.delete() }
        if (wasCapturing && isEnabled(context)) {
            // The capture thread exits asynchronously once logcat dies; start()
            // is idempotent and no-ops until it has, so re-arm on the next
            // start/share instead of racing it here.
            start(context)
        }
        return deleted
    }

    /**
     * Starts capturing (idempotent). Safe to call from both [MainActivity] and
     * [com.cruisemesh.app.mesh.MeshService]: whichever spins up the process
     * first starts the single capture thread; the other call is a no-op.
     */
    @Synchronized
    fun start(context: Context) {
        if (started || !isEnabled(context)) return
        started = true
        val appContext = context.applicationContext
        thread(name = "debug-file-log", isDaemon = true) {
            try {
                capture(appContext)
            } catch (e: Exception) {
                Log.w(TAG, "log capture stopped: ${e.message}")
            } finally {
                logcatProcess = null
                started = false
            }
        }
    }

    /**
     * Tears down the logcat child; the capture thread then sees end-of-stream
     * and exits through its normal cleanup path.
     */
    @Synchronized
    private fun stopCapture() {
        logcatProcess?.destroy()
    }

    private fun capture(context: Context) {
        val file = logFile(context)
        if (file.exists() && file.length() >= MAX_BYTES) rotate(context, file)

        val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US).format(Date())
        val version = try {
            val info = context.packageManager.getPackageInfo(context.packageName, 0)
            "${info.versionName} (${PackageInfoCompat.getLongVersionCode(info)})"
        } catch (e: Exception) {
            "unknown version"
        }
        file.appendText(
            "\n===== capture start $stamp pid=${Process.myPid()} " +
                "CruiseMesh $version " +
                "${Build.MANUFACTURER} ${Build.MODEL} Android ${Build.VERSION.RELEASE} =====\n",
        )
        // The device conditions that silently stop the mesh working, and why
        // the last process died -- neither is inferable from the log lines
        // themselves. See EnvironmentSnapshot and ProcessExitHistory.
        file.appendText(EnvironmentSnapshot.format(EnvironmentSnapshot.capture(context)))
        file.appendText(ProcessExitHistory.format(ProcessExitHistory.recentExits(context)))

        // -v threadtime keeps timestamps + tid; --pid restricts to us even on
        // the off chance the platform would hand back more.
        val process = ProcessBuilder(
            "logcat", "-v", "threadtime", "--pid=${Process.myPid()}",
        ).redirectErrorStream(true).start()
        logcatProcess = process
        // Re-check after publishing the process: an opt-out racing this
        // startup may have missed it in stopCapture (the pref write happens
        // before that null check, so one of the two sides always sees the
        // other's effect).
        if (!isEnabled(context)) {
            process.destroy()
        }

        process.inputStream.bufferedReader().use { reader ->
            var size = file.length()
            while (true) {
                val line = reader.readLine() ?: break
                file.appendText(line)
                file.appendText("\n")
                size += line.length + 1
                if (size >= MAX_BYTES) {
                    rotate(context, file)
                    size = 0
                }
            }
        }
    }

    private fun rotate(context: Context, file: File) {
        val rotated = File(logDir(context), ROTATED_NAME)
        if (rotated.exists()) rotated.delete()
        file.renameTo(rotated)
    }

    /**
     * A share [Intent] for the captured log, or null if nothing has been
     * written yet. Streams the current file via the existing FileProvider.
     */
    fun shareIntent(context: Context): Intent? {
        val file = logFile(context)
        if (!file.exists() || file.length() == 0L) return null
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
        return Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, "CruiseMesh debug log")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
    }
}
