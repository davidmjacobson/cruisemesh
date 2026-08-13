package com.cruisemesh.app.debug

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import com.cruisemesh.app.AppStore
import java.io.File
import java.time.LocalDate
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/**
 * Everything captured, in one share sheet.
 *
 * The connection log and the delivery-timings CSV answer different questions --
 * what the radios did, whether messages actually arrived and how fast, and
 * whether an ambiguous sender stream was quarantined. None is derivable from
 * the others. Splitting them across buttons meant that asking a family member
 * to "send diagnostics" reliably produced only part of the picture, and the
 * round trip to ask for the rest can cost a day at sea. So the tester-facing
 * surface has one button, and it sends them all.
 *
 * The granular per-file exports still exist on the internal tools screen, where
 * the point is analysis rather than a support hand-off.
 */
object DiagnosticsShare {
    private const val ARCHIVE_DIR = "diagnostics"

    /**
     * A share [Intent] carrying every captured artifact as one zip, or `null`
     * when there is nothing to send.
     *
     * One file, deliberately, even though `ACTION_SEND_MULTIPLE` is the
     * documented way to attach several. Sending both files that way is correct
     * on our side and the sheet even counts them ("Sharing 2 files"), but plenty
     * of receiving apps take the first attachment and silently drop the rest --
     * Files by Google saves only the log. The failure is invisible: the tester
     * believes they sent diagnostics, support gets half of them, and neither
     * side can tell. A zip cannot be half-consumed, and "send me the one file"
     * is a simpler thing to ask a family member for anyway.
     */
    fun shareIntent(context: Context): Intent? {
        val files = capturedFiles(context)
        if (files.isEmpty()) return null
        val archive = writeArchive(files, archiveFile(context))
        val uri = uriFor(context, archive ?: files.first())
        // Drive (and most other targets) only open this URI after the user
        // picks a folder or hits send. That open is what the confirmation
        // toast waits on -- the chooser result fires too early.
        DiagnosticsShareHandoff.expect(uri.toString())
        return Intent(Intent.ACTION_SEND).apply {
            if (archive != null) {
                type = "application/zip"
                putExtra(Intent.EXTRA_STREAM, uri)
            } else {
                // Zipping is a disk write and can fail -- a full device, most
                // likely. Sending the log alone beats telling someone who has
                // captured diagnostics that they have none.
                type = mimeFor(files.first())
                putExtra(Intent.EXTRA_STREAM, uri)
            }
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
        val store = AppStore.get(context)
        if (runCatching { store.hasDeliveryMetrics() }.getOrNull() == true) return true
        if (runCatching { store.hasMessageConflicts() }.getOrNull() == true) return true
        return ProtocolEventExport.hasCapturedEvents(context)
    }

    /**
     * Erases the zip written by the last share.
     *
     * The archive is a full second copy of the log and the metrics, so a
     * "delete captured diagnostics" that left it sitting in external files
     * would be untrue. Mirrors [FieldMetricsExport.deleteCsvFile].
     */
    fun deleteArchive(context: Context) {
        archiveDir(context).listFiles()?.forEach { it.delete() }
    }

    /**
     * The captured files, in the order a reader wants them: the log first,
     * since it is the narrative, then the delivery and conflict CSVs.
     *
     * Writing the CSV is a side effect of asking -- it is regenerated from the
     * core on each call rather than kept on disk -- so this is only ever called
     * from the share path, never from composition. See [hasAnythingCaptured].
     */
    private fun capturedFiles(context: Context): List<File> {
        val files = mutableListOf<File>()
        DebugFileLog.logFile(context).takeIf { it.exists() && it.length() > 0 }?.let(files::add)
        FieldMetricsExport.writeCsvFile(context)?.let(files::add)
        ConflictDiagnosticsExport.writeCsvFile(context)?.let(files::add)
        ProtocolEventExport.writeJsonlFile(context)?.let(files::add)
        return files
    }

    /**
     * Zips [files] into [dest], returning it, or `null` if the write failed.
     *
     * Entry names are the plain file names: a zip whose entries carry the
     * device's directory layout is harder to read and leaks paths for no gain.
     * Context-free so it can be unit-tested against a temp directory.
     */
    internal fun writeArchive(files: List<File>, dest: File): File? = runCatching {
        dest.parentFile?.mkdirs()
        ZipOutputStream(dest.outputStream().buffered()).use { zip ->
            for (file in files) {
                zip.putNextEntry(ZipEntry(file.name))
                file.inputStream().buffered().use { it.copyTo(zip) }
                zip.closeEntry()
            }
        }
        dest
    }.getOrElse {
        // A half-written zip is worse than none: it would share as a plausible
        // attachment and fail to open on the far side.
        dest.delete()
        null
    }

    /**
     * Where this share's zip goes. Dated, because the first thing anyone asks
     * of a diagnostics file is when it was taken, and the file name is the only
     * part of it that survives being forwarded through three apps.
     *
     * The directory is cleared first so a week of shares does not accumulate
     * copies of the log next to the log.
     */
    private fun archiveFile(context: Context): File {
        deleteArchive(context)
        return File(archiveDir(context), "cruisemesh-diagnostics-${LocalDate.now()}.zip")
    }

    private fun archiveDir(context: Context): File =
        File(context.getExternalFilesDir(null), ARCHIVE_DIR).apply { mkdirs() }

    private fun uriFor(context: Context, file: File): Uri =
        FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)

    private fun mimeFor(file: File): String =
        if (file.name.endsWith(".csv")) "text/csv" else "text/plain"
}
