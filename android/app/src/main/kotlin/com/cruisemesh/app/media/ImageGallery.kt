package com.cruisemesh.app.media

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.util.Log

/**
 * Saves inline chat JPEGs into the device gallery (Pictures/CruiseMesh)
 * via [MediaStore] so no legacy storage permission is needed on API 29+.
 */
object ImageGallery {
    private const val TAG = "ImageGallery"

    fun saveJpeg(context: Context, jpeg: ByteArray): Uri? {
        val name = "CruiseMesh_${System.currentTimeMillis()}.jpg"
        return try {
            val resolver = context.contentResolver
            val values = ContentValues().apply {
                put(MediaStore.Images.Media.DISPLAY_NAME, name)
                put(MediaStore.Images.Media.MIME_TYPE, "image/jpeg")
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    put(
                        MediaStore.Images.Media.RELATIVE_PATH,
                        Environment.DIRECTORY_PICTURES + "/CruiseMesh",
                    )
                    put(MediaStore.Images.Media.IS_PENDING, 1)
                }
            }
            val collection = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
            } else {
                MediaStore.Images.Media.EXTERNAL_CONTENT_URI
            }
            val uri = resolver.insert(collection, values) ?: return null
            try {
                resolver.openOutputStream(uri)?.use { out ->
                    out.write(jpeg)
                } ?: run {
                    resolver.delete(uri, null, null)
                    return null
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    values.clear()
                    values.put(MediaStore.Images.Media.IS_PENDING, 0)
                    resolver.update(uri, values, null, null)
                }
                uri
            } catch (e: Exception) {
                resolver.delete(uri, null, null)
                throw e
            }
        } catch (e: Exception) {
            Log.w(TAG, "Failed to save image: ${e.message}")
            null
        }
    }

    /**
     * Size a source image into [maxWidth]×[maxHeight] while preserving aspect
     * ratio (letterbox-style fit, never crop).
     */
    fun fitSize(
        sourceWidth: Int,
        sourceHeight: Int,
        maxWidth: Float,
        maxHeight: Float,
    ): Pair<Float, Float> {
        if (sourceWidth <= 0 || sourceHeight <= 0 || maxWidth <= 0f || maxHeight <= 0f) {
            return 0f to 0f
        }
        val aspect = sourceWidth.toFloat() / sourceHeight.toFloat()
        var width = maxWidth
        var height = width / aspect
        if (height > maxHeight) {
            height = maxHeight
            width = height * aspect
        }
        return width to height
    }
}
