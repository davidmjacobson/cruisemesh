package com.cruisemesh.app.media

import android.content.Context
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File

/**
 * A fresh file:// target (exposed as a content:// Uri via FileProvider) for a
 * full-resolution camera capture. Callers must use
 * [androidx.activity.result.contract.ActivityResultContracts.TakePicture]
 * (Uri in, Boolean success out) rather than `TakePicturePreview` (Bitmap
 * out) -- the latter ships the full-size photo through a Binder transaction
 * and throws `TransactionTooLargeException` on modern high-megapixel
 * cameras.
 */
fun createCameraCaptureUri(context: Context): Uri {
    val dir = File(context.cacheDir, "camera").apply { mkdirs() }
    val file = File(dir, "capture-${System.currentTimeMillis()}.jpg")
    return FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
}
