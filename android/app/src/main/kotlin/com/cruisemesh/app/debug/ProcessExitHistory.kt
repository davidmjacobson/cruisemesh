package com.cruisemesh.app.debug

import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.content.Context
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Why previous processes died, recorded at the top of each capture session.
 *
 * [DebugFileLog] streams this process's own logcat, which means the record of
 * a death is only as good as what the dying process managed to emit. A Java
 * crash does land in logcat (the runtime writes the stack before the process
 * goes), but an ANR, a low-memory kill, or a native crash leaves nothing but a
 * gap followed by a fresh capture-start banner -- indistinguishable, in the
 * shared file, from the user force-quitting the app. `ActivityManager` keeps
 * the answer for us across process deaths, so ask it once per launch.
 *
 * Metadata only, in keeping with the rest of the diagnostics: reason codes,
 * timestamps, and the OS-authored description. Traces are pulled only for the
 * cases logcat cannot cover (ANR and native crash), where the stack is the
 * whole point.
 */
object ProcessExitHistory {
    /** How many prior deaths to report. Enough to see a crash loop. */
    private const val MAX_EXITS = 5

    /** Cap on any single attached trace; ANR traces can run to megabytes. */
    private const val MAX_TRACE_BYTES = 64 * 1024

    /**
     * One prior process death, reduced to the fields worth writing down.
     * Kept free of Android types so [format] is unit-testable on the JVM.
     */
    data class Exit(
        val timestampMs: Long,
        val reasonLabel: String,
        val description: String?,
        val trace: String?,
    )

    /**
     * Renders exits newest-first. Returns an empty string for an empty list so
     * a first-ever launch adds nothing rather than a puzzling "no exits" line.
     */
    fun format(exits: List<Exit>): String {
        if (exits.isEmpty()) return ""
        val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US)
        val out = StringBuilder("----- previous process exits (newest first) -----\n")
        for (exit in exits) {
            out.append("  ${stamp.format(Date(exit.timestampMs))}  ${exit.reasonLabel}")
            exit.description?.takeIf { it.isNotBlank() }?.let { out.append("  — $it") }
            out.append("\n")
            exit.trace?.takeIf { it.isNotBlank() }?.let { trace ->
                out.append("  --- trace ---\n")
                trace.lineSequence().forEach { out.append("  $it\n") }
                out.append("  --- end trace ---\n")
            }
        }
        out.append("----- end previous process exits -----\n")
        return out.toString()
    }

    /** Reads the platform's exit records for this app. */
    fun recentExits(context: Context): List<Exit> {
        val am = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
            ?: return emptyList()
        val infos = try {
            am.getHistoricalProcessExitReasons(context.packageName, 0, MAX_EXITS)
        } catch (e: Exception) {
            return emptyList()
        }
        return infos.map { info ->
            Exit(
                timestampMs = info.timestamp,
                reasonLabel = reasonLabel(info.reason),
                description = info.description,
                trace = traceFor(info),
            )
        }
    }

    /**
     * The trace, but only where logcat would not already have it: ANRs and
     * native crashes. A Java crash's stack is written to logcat by the runtime
     * before the process dies, so [DebugFileLog] already captured it and
     * duplicating it here would just push older entries out of the file.
     */
    private fun traceFor(info: ApplicationExitInfo): String? {
        if (info.reason != ApplicationExitInfo.REASON_ANR &&
            info.reason != ApplicationExitInfo.REASON_CRASH_NATIVE
        ) {
            return null
        }
        return try {
            info.traceInputStream?.use { stream ->
                val bytes = ByteArray(MAX_TRACE_BYTES)
                var read = 0
                while (read < MAX_TRACE_BYTES) {
                    val n = stream.read(bytes, read, MAX_TRACE_BYTES - read)
                    if (n <= 0) break
                    read += n
                }
                if (read <= 0) {
                    null
                } else {
                    val text = String(bytes, 0, read)
                    if (read >= MAX_TRACE_BYTES) "$text\n[trace truncated]" else text
                }
            }
        } catch (e: Exception) {
            null
        }
    }

    /** Human-readable name for an [ApplicationExitInfo] reason code. */
    fun reasonLabel(reason: Int): String = when (reason) {
        ApplicationExitInfo.REASON_ANR -> "ANR (app not responding)"
        ApplicationExitInfo.REASON_CRASH -> "CRASH (unhandled exception)"
        ApplicationExitInfo.REASON_CRASH_NATIVE -> "CRASH (native)"
        ApplicationExitInfo.REASON_DEPENDENCY_DIED -> "dependency died"
        ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> "killed for resource usage"
        ApplicationExitInfo.REASON_EXIT_SELF -> "exited itself"
        ApplicationExitInfo.REASON_FREEZER -> "frozen by the OS"
        ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> "initialization failure"
        ApplicationExitInfo.REASON_LOW_MEMORY -> "killed for low memory"
        ApplicationExitInfo.REASON_OTHER -> "other"
        ApplicationExitInfo.REASON_PACKAGE_STATE_CHANGE -> "package state change"
        ApplicationExitInfo.REASON_PACKAGE_UPDATED -> "app updated"
        ApplicationExitInfo.REASON_PERMISSION_CHANGE -> "permission change"
        ApplicationExitInfo.REASON_SIGNALED -> "killed by signal"
        ApplicationExitInfo.REASON_USER_REQUESTED -> "user swiped it away"
        ApplicationExitInfo.REASON_USER_STOPPED -> "user stopped it"
        else -> "unknown ($reason)"
    }
}
