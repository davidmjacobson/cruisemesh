package com.cruisemesh.app.media

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.net.Uri
import android.util.Log
import java.io.ByteArrayOutputStream

private const val TAG = "MediaCompressor"

/**
 * Downscales and JPEG-compresses a gallery/camera image so it fits the
 * inline attachment budget ([AttachmentPayload.MAX_BLOB_BYTES]).
 */
object MediaCompressor {
    private const val MAX_EDGE_PX = 1280
    private const val MIN_QUALITY = 40
    private const val START_QUALITY = 82

    fun compressImageUri(context: Context, uri: Uri): ByteArray? {
        return try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            context.contentResolver.openInputStream(uri)?.use {
                BitmapFactory.decodeStream(it, null, bounds)
            } ?: return null

            val sample = sampleSizeFor(bounds.outWidth, bounds.outHeight, MAX_EDGE_PX)
            val opts = BitmapFactory.Options().apply { inSampleSize = sample }
            val decoded = context.contentResolver.openInputStream(uri)?.use {
                BitmapFactory.decodeStream(it, null, opts)
            } ?: return null

            val scaled = scaleToMaxEdge(decoded, MAX_EDGE_PX)
            if (scaled !== decoded) decoded.recycle()
            compressJpeg(scaled).also { scaled.recycle() }
        } catch (e: Exception) {
            Log.w(TAG, "Failed to compress image from $uri: ${e.message}")
            null
        }
    }

    fun compressImageFile(path: String): ByteArray? {
        return try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeFile(path, bounds)
            val sample = sampleSizeFor(bounds.outWidth, bounds.outHeight, MAX_EDGE_PX)
            val opts = BitmapFactory.Options().apply { inSampleSize = sample }
            val decoded = BitmapFactory.decodeFile(path, opts) ?: return null
            val scaled = scaleToMaxEdge(decoded, MAX_EDGE_PX)
            if (scaled !== decoded) decoded.recycle()
            compressJpeg(scaled).also { scaled.recycle() }
        } catch (e: Exception) {
            Log.w(TAG, "Failed to compress image file $path: ${e.message}")
            null
        }
    }

    private fun sampleSizeFor(width: Int, height: Int, maxEdge: Int): Int {
        var sample = 1
        var w = width
        var h = height
        while (w / 2 >= maxEdge || h / 2 >= maxEdge) {
            sample *= 2
            w /= 2
            h /= 2
        }
        return sample.coerceAtLeast(1)
    }

    private fun scaleToMaxEdge(source: Bitmap, maxEdge: Int): Bitmap {
        val longest = maxOf(source.width, source.height)
        if (longest <= maxEdge) return source
        val scale = maxEdge.toFloat() / longest.toFloat()
        val matrix = Matrix().apply { setScale(scale, scale) }
        return Bitmap.createBitmap(source, 0, 0, source.width, source.height, matrix, true)
    }

    private fun compressJpeg(bitmap: Bitmap): ByteArray? {
        var quality = START_QUALITY
        var bytes: ByteArray
        do {
            val stream = ByteArrayOutputStream()
            if (!bitmap.compress(Bitmap.CompressFormat.JPEG, quality, stream)) {
                return null
            }
            bytes = stream.toByteArray()
            if (bytes.size <= AttachmentPayload.MAX_BLOB_BYTES) {
                return bytes
            }
            quality -= 8
        } while (quality >= MIN_QUALITY)

        Log.w(TAG, "Image still ${bytes.size} bytes after min quality; refusing")
        return null
    }
}
