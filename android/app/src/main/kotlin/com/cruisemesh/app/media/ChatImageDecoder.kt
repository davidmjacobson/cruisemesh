package com.cruisemesh.app.media

import android.graphics.Bitmap
import android.graphics.BitmapFactory

/**
 * Downsampled JPEG decoding for chat bubbles / pending-photo previews (FA4).
 * Chat images are stored inline at up to ~1024px on their long edge
 * ([MediaCompressor]), but bubbles only ever render at a few hundred dp; a
 * full-resolution [BitmapFactory.decodeByteArray] call for every visible
 * bubble is wasted allocation and, worse, was happening synchronously during
 * composition. Callers decode via [decodeSampled] off the main thread and
 * target the actual display size.
 */
object ChatImageDecoder {
    /** Reads just the JPEG header (dimensions) -- cheap, no pixel buffer allocated. */
    fun decodeBounds(bytes: ByteArray): Pair<Int, Int>? {
        val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
        return if (options.outWidth > 0 && options.outHeight > 0) {
            options.outWidth to options.outHeight
        } else {
            null
        }
    }

    /**
     * Decodes at the smallest power-of-two sample size whose result is still
     * at least [reqWidth]x[reqHeight] -- the decoded bitmap ends up between 1x
     * and 2x the requested size, never full source resolution for a bubble
     * that only displays at [reqWidth]x[reqHeight].
     */
    fun decodeSampled(bytes: ByteArray, reqWidth: Int, reqHeight: Int): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        val options = BitmapFactory.Options().apply {
            inSampleSize = sampleSizeFor(bounds.outWidth, bounds.outHeight, reqWidth, reqHeight)
        }
        return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
    }

    /**
     * Standard Android power-of-two `inSampleSize` search: doubles until
     * halving again would undershoot the requested size. Pure function so it
     * can be unit-tested without a real (or Robolectric) [BitmapFactory].
     */
    internal fun sampleSizeFor(width: Int, height: Int, reqWidth: Int, reqHeight: Int): Int {
        var sampleSize = 1
        if (width <= 0 || height <= 0 || reqWidth <= 0 || reqHeight <= 0) return sampleSize
        if (height > reqHeight || width > reqWidth) {
            var halfHeight = height / 2
            var halfWidth = width / 2
            while (halfHeight / sampleSize >= reqHeight && halfWidth / sampleSize >= reqWidth) {
                sampleSize *= 2
            }
        }
        return sampleSize
    }
}
