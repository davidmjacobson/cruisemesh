package com.cruisemesh.app.debug

import android.content.Context
import com.cruisemesh.app.AppStore
import java.io.File

/**
 * Writes the core's protocol-event ring into the shared diagnostics archive.
 *
 * Rust owns everything about the file: the schema, what may appear in a
 * record, the archive-local pseudonyms that stand in for contacts and
 * mailboxes, and the export itself. This class decides nothing -- it asks the
 * store for a string and puts it on disk. That is deliberate: the ring exists
 * so that a support hand-off carries what the device actually decided, and a
 * shell that reformatted or filtered it on the way out would be one more place
 * for the two platforms to disagree.
 *
 * Nothing here uploads or schedules anything. The file is written only when
 * someone taps share, and "delete captured diagnostics" clears the ring as
 * well as this copy of it.
 */
object ProtocolEventExport {
    private const val DIAGNOSTICS_DIR = "metrics"
    private const val FILE_NAME = "cruisemesh-protocol-events.jsonl"

    /** A freshly exported ring, or `null` when the core has nothing to say. */
    fun writeJsonlFile(context: Context): File? {
        val store = AppStore.get(context)
        if (runCatching { store.hasProtocolEvents() }.getOrNull() != true) return null
        val jsonl = runCatching { store.exportProtocolEventsJsonl() }.getOrNull() ?: return null
        // A header with no records is a file that answers nothing; the reader
        // is better served by its absence than by an empty archive.
        if (jsonl.trim().lineSequence().count() <= 1) return null

        val dir = File(context.getExternalFilesDir(null), DIAGNOSTICS_DIR).apply { mkdirs() }
        return File(dir, FILE_NAME).also { it.writeText(jsonl) }
    }

    /** Whether the ring holds anything, for gating the share and delete buttons. */
    fun hasCapturedEvents(context: Context): Boolean =
        runCatching { AppStore.get(context).hasProtocolEvents() }.getOrNull() ?: false

    /**
     * Removes the exported copy. The ring itself is cleared separately, in the
     * same block that clears the metrics tables -- both have to go, or the next
     * share would rebuild the file that was just deleted.
     */
    fun deleteJsonlFile(context: Context) {
        File(File(context.getExternalFilesDir(null), DIAGNOSTICS_DIR), FILE_NAME).delete()
    }
}
