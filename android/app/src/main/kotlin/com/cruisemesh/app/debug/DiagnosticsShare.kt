package com.cruisemesh.app.debug

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import com.cruisemesh.app.AppStore
import java.io.File

/**
 * Everything captured, in one share sheet.
 *
 * The connection log and the delivery-timings CSV answer different questions --
 * what the radios did, versus whether messages actually arrived and how fast --
 * and neither is derivable from the other. Splitting them across two buttons
 * meant that asking a family member to "send diagnostics" reliably produced
 * half the picture, and the round trip to ask for the other half can cost a day
 * at sea. So the tester-facing surface has one button, and it sends both.
 *
 * The granular per-file exports still exist on the internal tools screen, where
 * the point is analysis rather than a support hand-off.
 */
object DiagnosticsShare {
    /**
     * A share [Intent] carrying every captured artifact, or `null` when there
     * is nothing to send. Uses `ACTION_SEND_MULTIPLE` whenever more than one
     * file exists; a lone file still goes as a plain `ACTION_SEND`, which more
     * targets accept.
     */
    fun shareIntent(context: Context): Intent? {
        val files = capturedFiles(context)
        if (files.isEmpty()) return null
        val uris = files.map { uriFor(context, it) }
        val intent = if (uris.size == 1) {
            Intent(Intent.ACTION_SEND).apply { putExtra(Intent.EXTRA_STREAM, uris.first()) }
        } else {
            Intent(Intent.ACTION_SEND_MULTIPLE).apply {
                putParcelableArrayListExtra(Intent.EXTRA_STREAM, ArrayList(uris))
            }
        }
        return intent.apply {
            // Mixed text/csv: the only honest common ancestor is */*. Naming
            // text/plain here would hide the CSV from targets that filter on it.
            type = if (files.size == 1) mimeFor(files.first()) else "*/*"
            putExtra(Intent.EXTRA_SUBJECT, "CruiseMesh diagnostics")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
    }

    /**
     * Whether anything at all has been captured, for gating the delete button.
     *
     * Deliberately does NOT go through [capturedFiles]: that materialises the
     * CSV on disk, and this runs during Compose composition, where it would
     * mean a SQLite export plus a file write on the main thread every time the
     * screen is opened -- and would recreate the very file the delete button
     * had just removed. [AppStore.hasDeliveryMetrics] stops at the first row
     * and touches nothing.
     */
    fun hasAnythingCaptured(context: Context): Boolean {
        if (DebugFileLog.hasCapturedLogs(context)) return true
        return runCatching { AppStore.get(context).hasDeliveryMetrics() }.getOrNull() ?: false
    }

    /**
     * The captured files, in the order a reader wants them: the log first,
     * since it is the narrative, then the metrics CSV.
     *
     * Writing the CSV is a side effect of asking -- it is regenerated from the
     * core on each call rather than kept on disk -- so this is only ever called
     * from the share path, never from composition. See [hasAnythingCaptured].
     */
    private fun capturedFiles(context: Context): List<File> {
        val files = mutableListOf<File>()
        DebugFileLog.logFile(context).takeIf { it.exists() && it.length() > 0 }?.let(files::add)
        FieldMetricsExport.writeCsvFile(context)?.let(files::add)
        return files
    }

    private fun uriFor(context: Context, file: File): Uri =
        FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)

    private fun mimeFor(file: File): String =
        if (file.name.endsWith(".csv")) "text/csv" else "text/plain"
}
