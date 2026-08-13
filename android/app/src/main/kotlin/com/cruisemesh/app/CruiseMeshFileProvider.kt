package com.cruisemesh.app

import android.net.Uri
import android.os.ParcelFileDescriptor
import androidx.core.content.FileProvider
import com.cruisemesh.app.debug.DiagnosticsShareHandoff

/**
 * App FileProvider. Same paths as the stock one; the override exists so
 * "Share diagnostics" can see the moment Drive (or another target) actually
 * reads the archive. The share sheet itself never reports that.
 */
class CruiseMeshFileProvider : FileProvider() {
    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor? {
        val descriptor = super.openFile(uri, mode)
        if (descriptor != null) {
            DiagnosticsShareHandoff.onOpened(uri.toString())
        }
        return descriptor
    }
}
