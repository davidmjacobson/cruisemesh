package com.cruisemesh.app.media

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.net.Uri
import android.util.Log
import androidx.exifinterface.media.ExifInterface
import java.io.ByteArrayOutputStream

private const val TAG = "MediaCompressor"

/**
 * Downscales and JPEG-compresses a gallery/camera image so it fits the
 * inline attachment budget ([AttachmentPayload.MAX_BLOB_BYTES]).
 */
object MediaCompressor {
    private const val MAX_EDGE_PX = 1024
    private const val MIN_QUALITY = 40
    private const val START_QUALITY = 75

    /**
     * Compression aims for this, well under [AttachmentPayload.MAX_BLOB_BYTES].
     * The hard cap (180 KB) is a terrible transfer target: at ~510 bytes/BLE
     * fragment that's ~360 fragments, each a ~150 ms write-with-response, so a
     * photo took ~1 min to send. ~48 KB is ~95 fragments (~15 s) and still
     * looks fine at 1024 px on a phone screen. MAX_BLOB_BYTES stays only as a
     * last-resort ceiling for an unusually detailed image.
     */
    private const val TARGET_BLOB_BYTES = 48 * 1024

    fun compressImageUri(context: Context, uri: Uri): ByteArray? {
        return try {
            // A bounds-only pass (inJustDecodeBounds) always returns a null Bitmap
            // by design -- it only fills in outWidth/outHeight. So guard the *stream*
            // itself, and ignore the (expected) null decode result; otherwise the
            // elvis below would misfire on every image and nothing would ever send.
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            (context.contentResolver.openInputStream(uri) ?: return null).use {
                BitmapFactory.decodeStream(it, null, bounds)
            }
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null

            val sample = sampleSizeFor(bounds.outWidth, bounds.outHeight, MAX_EDGE_PX)
            val opts = BitmapFactory.Options().apply { inSampleSize = sample }
            val decoded = (context.contentResolver.openInputStream(uri) ?: return null).use {
                BitmapFactory.decodeStream(it, null, opts)
            } ?: return null

            // Camera photos record their rotation only in EXIF; re-compressing to
            // JPEG below drops that tag, so bake the orientation into the pixels.
            val orientation = context.contentResolver.openInputStream(uri)?.use {
                ExifInterface(it).getAttributeInt(
                    ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL,
                )
            } ?: ExifInterface.ORIENTATION_NORMAL

            val transformed = transform(decoded, MAX_EDGE_PX, orientation)
            if (transformed !== decoded) decoded.recycle()
            compressJpeg(transformed).also { transformed.recycle() }
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
            val orientation = ExifInterface(path).getAttributeInt(
                ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL,
            )
            val transformed = transform(decoded, MAX_EDGE_PX, orientation)
            if (transformed !== decoded) decoded.recycle()
            compressJpeg(transformed).also { transformed.recycle() }
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

    /**
     * Downscales to [maxEdge] and applies the source's EXIF [orientation] in a
     * single matrix pass. Returns [source] unchanged when neither is needed.
     */
    private fun transform(source: Bitmap, maxEdge: Int, orientation: Int): Bitmap {
        val longest = maxOf(source.width, source.height)
        val scale = if (longest > maxEdge) maxEdge.toFloat() / longest.toFloat() else 1f
        val matrix = orientationMatrix(orientation) ?: Matrix()
        if (scale != 1f) matrix.postScale(scale, scale)
        if (matrix.isIdentity) return source
        return Bitmap.createBitmap(source, 0, 0, source.width, source.height, matrix, true)
    }

    /** Rotation/flip for an EXIF orientation tag, or null if none is required. */
    private fun orientationMatrix(orientation: Int): Matrix? {
        val matrix = Matrix()
        when (orientation) {
            ExifInterface.ORIENTATION_ROTATE_90 -> matrix.postRotate(90f)
            ExifInterface.ORIENTATION_ROTATE_180 -> matrix.postRotate(180f)
            ExifInterface.ORIENTATION_ROTATE_270 -> matrix.postRotate(270f)
            ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> matrix.postScale(-1f, 1f)
            ExifInterface.ORIENTATION_FLIP_VERTICAL -> matrix.postScale(1f, -1f)
            ExifInterface.ORIENTATION_TRANSPOSE -> {
                matrix.postRotate(90f)
                matrix.postScale(-1f, 1f)
            }
            ExifInterface.ORIENTATION_TRANSVERSE -> {
                matrix.postRotate(270f)
                matrix.postScale(-1f, 1f)
            }
            else -> return null
        }
        return matrix
    }

    private fun compressJpeg(bitmap: Bitmap): ByteArray? {
        var quality = START_QUALITY
        var smallest: ByteArray? = null
        do {
            val stream = ByteArrayOutputStream()
            if (!bitmap.compress(Bitmap.CompressFormat.JPEG, quality, stream)) {
                break
            }
            val bytes = stream.toByteArray()
            if (smallest == null || bytes.size < smallest.size) smallest = bytes
            // Small enough for a fast BLE transfer -- good enough, stop shrinking.
            if (bytes.size <= TARGET_BLOB_BYTES) {
                return bytes
            }
            quality -= 8
        } while (quality >= MIN_QUALITY)

        // Couldn't reach the target even at MIN_QUALITY. Still send the smallest
        // we produced as long as it fits the hard blob cap; a very detailed photo
        // just costs more fragments. Only refuse if it can't fit at all.
        return smallest?.takeIf { it.size <= AttachmentPayload.MAX_BLOB_BYTES }
            ?: run {
                Log.w(TAG, "Image still ${smallest?.size} bytes after min quality; refusing")
                null
            }
    }
}
