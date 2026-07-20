package com.cruisemesh.app.debug

import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import com.cruisemesh.app.AppStore
import java.io.File

/**
 * V2 field metrics: turns the core's delivery-metrics CSV into a shareable
 * file for the cruise test. Metadata only -- the CSV carries hashed chat tags,
 * lamports, transports, and timings, never message content or raw contact ids
 * (see the core `delivery_metrics` table). Mirrors [DebugFileLog.shareIntent].
 */
object FieldMetricsExport {
    private const val METRICS_DIR = "metrics"
    private const val FILE_NAME = "cruisemesh-field-metrics.csv"

    /**
     * Writes the current metrics to a shareable file and returns a share
     * [Intent], or `null` when nothing has been captured yet (header only).
     */
    fun shareIntent(context: Context): Intent? {
        val csv = runCatching { AppStore.get(context).exportDeliveryMetricsCsv() }.getOrNull()
            ?: return null
        // A header line with no data rows means there's nothing to share.
        if (csv.trim().lineSequence().count() <= 1) return null

        val dir = File(context.getExternalFilesDir(null), METRICS_DIR).apply { mkdirs() }
        val file = File(dir, FILE_NAME)
        file.writeText(csv)

        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
        return Intent(Intent.ACTION_SEND).apply {
            type = "text/csv"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, "CruiseMesh field metrics")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
    }
}
