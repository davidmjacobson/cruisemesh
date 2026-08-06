package com.cruisemesh.app.debug

import android.content.Context
import com.cruisemesh.app.AppStore
import java.io.File

/**
 * Writes Rust-owned, metadata-only stream-conflict summaries for the shared
 * diagnostics archive. The core replaces message content and raw identities
 * with stable hashes before the data crosses the UniFFI boundary.
 */
object ConflictDiagnosticsExport {
    private const val DIAGNOSTICS_DIR = "metrics"
    private const val FILE_NAME = "cruisemesh-message-conflicts.csv"

    /** Returns a freshly generated CSV, or `null` when quarantine is empty. */
    fun writeCsvFile(context: Context): File? {
        val csv = runCatching { AppStore.get(context).exportMessageConflictsCsv() }.getOrNull()
            ?: return null
        if (csv.trim().lineSequence().count() <= 1) return null

        val dir = File(context.getExternalFilesDir(null), DIAGNOSTICS_DIR).apply { mkdirs() }
        return File(dir, FILE_NAME).also { it.writeText(csv) }
    }

    /** Removes the last exported copy; the core quarantine is cleared separately. */
    fun deleteCsvFile(context: Context) {
        File(File(context.getExternalFilesDir(null), DIAGNOSTICS_DIR), FILE_NAME).delete()
    }
}
