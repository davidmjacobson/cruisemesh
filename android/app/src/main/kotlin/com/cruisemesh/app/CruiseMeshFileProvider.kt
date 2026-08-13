package com.cruisemesh.app

import android.net.Uri
import android.os.ParcelFileDescriptor
import androidx.core.content.FileProvider
import com.cruisemesh.app.debug.DiagnosticsShareHandoff

/** FileProvider that reports a third-party read of a diagnostics share. */
class CruiseMeshFileProvider : FileProvider() {
    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor? {
        val descriptor = super.openFile(uri, mode) ?: return null
        val caller = runCatching { callingPackage }.getOrNull()
        DiagnosticsShareHandoff.onOpened(
            key = uri.toString(),
            mode = mode,
            callerPackage = caller,
            ownPackage = context?.packageName,
        )
        return descriptor
    }
}
