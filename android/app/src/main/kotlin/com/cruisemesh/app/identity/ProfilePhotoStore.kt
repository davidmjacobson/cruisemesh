package com.cruisemesh.app.identity

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.net.Uri
import android.util.Log
import androidx.exifinterface.media.ExifInterface
import java.io.File
import java.io.FileOutputStream
import java.io.ByteArrayOutputStream
import kotlin.math.max

private const val TAG = "ProfilePhotoStore"
private const val PROFILE_DIR = "profile"
private const val PROFILE_PHOTO_FILE = "avatar.jpg"
private const val PROFILE_PHOTO_EDGE = 512
private const val WIRE_PHOTO_EDGE = 256
private const val WIRE_PHOTO_MAX_BYTES = 24 * 1024

/** Persists the local profile photo shown in onboarding and the app chrome. */
object ProfilePhotoStore {

    fun loadAvatarPath(context: Context): String? =
        avatarFile(context).takeIf { it.isFile && it.length() > 0L }?.absolutePath

    fun clear(context: Context) {
        avatarFile(context).delete()
    }

    fun saveFromUri(context: Context, uri: Uri): String? {
        val bitmap = decodeSampledBitmap(context, uri) ?: return null
        return try {
            saveBitmap(context, bitmap)
        } finally {
            bitmap.recycle()
        }
    }

    /** Persist an already-decoded bitmap (e.g. tests or preview captures). */
    fun saveFromBitmap(context: Context, bitmap: Bitmap): String? = saveBitmap(context, bitmap)

    fun loadWireAvatarBytes(context: Context): ByteArray {
        val path = loadAvatarPath(context) ?: return ByteArray(0)
        val bitmap = BitmapFactory.decodeFile(path) ?: return ByteArray(0)
        return try {
            encodeWireAvatar(bitmap)
        } finally {
            bitmap.recycle()
        }
    }

    fun encodeWireAvatar(bitmap: Bitmap): ByteArray {
        val normalized = bitmap.centerCrop(WIRE_PHOTO_EDGE)
        try {
            for (quality in listOf(85, 70, 55, 40)) {
                val out = ByteArrayOutputStream()
                check(normalized.compress(Bitmap.CompressFormat.JPEG, quality, out))
                val bytes = out.toByteArray()
                if (bytes.size <= WIRE_PHOTO_MAX_BYTES || quality == 40) {
                    return if (bytes.size <= WIRE_PHOTO_MAX_BYTES) bytes else ByteArray(0)
                }
            }
            return ByteArray(0)
        } finally {
            if (normalized !== bitmap) normalized.recycle()
        }
    }

    private fun saveBitmap(context: Context, bitmap: Bitmap): String? =
        runCatching {
            val normalized = bitmap.centerCrop(PROFILE_PHOTO_EDGE)
            try {
                val file = avatarFile(context)
                file.parentFile?.mkdirs()
                FileOutputStream(file).use { output ->
                    check(normalized.compress(Bitmap.CompressFormat.JPEG, 90, output))
                }
                file.absolutePath
            } finally {
                if (normalized !== bitmap) normalized.recycle()
            }
        }.onFailure { e ->
            Log.w(TAG, "Failed to save profile photo: ${e.message}")
        }.getOrNull()

    private fun decodeSampledBitmap(context: Context, uri: Uri): Bitmap? {
        return try {
            // Bounds-only pass: decodeStream returns null by design when
            // inJustDecodeBounds is true. Guard the *stream*, not the bitmap,
            // or every save looks like a failure (same footgun MediaCompressor
            // already documents).
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            (context.contentResolver.openInputStream(uri) ?: return null).use { stream ->
                BitmapFactory.decodeStream(stream, null, bounds)
            }
            val maxDimension = max(bounds.outWidth, bounds.outHeight)
            if (maxDimension <= 0) return null

            var sampleSize = 1
            while (maxDimension / sampleSize > PROFILE_PHOTO_EDGE * 2) {
                sampleSize *= 2
            }

            val decodeOptions = BitmapFactory.Options().apply { inSampleSize = sampleSize }
            val decoded = (context.contentResolver.openInputStream(uri) ?: return null).use { stream ->
                BitmapFactory.decodeStream(stream, null, decodeOptions)
            } ?: return null

            // Camera photos store rotation in EXIF only; re-encoding to JPEG
            // drops that tag, so bake orientation into the pixels before crop.
            val orientation = context.contentResolver.openInputStream(uri)?.use { stream ->
                ExifInterface(stream).getAttributeInt(
                    ExifInterface.TAG_ORIENTATION,
                    ExifInterface.ORIENTATION_NORMAL,
                )
            } ?: ExifInterface.ORIENTATION_NORMAL

            val oriented = applyOrientation(decoded, orientation)
            if (oriented !== decoded) decoded.recycle()
            oriented
        } catch (e: Exception) {
            Log.w(TAG, "Failed to decode profile photo from $uri: ${e.message}")
            null
        }
    }

    private fun avatarFile(context: Context): File =
        context.applicationContext.filesDir.resolve(PROFILE_DIR).resolve(PROFILE_PHOTO_FILE)

    private fun Bitmap.centerCrop(edge: Int): Bitmap {
        val squareEdge = minOf(width, height)
        val x = (width - squareEdge) / 2
        val y = (height - squareEdge) / 2
        val square = Bitmap.createBitmap(this, x, y, squareEdge, squareEdge)
        return if (square.width == edge && square.height == edge) {
            square
        } else {
            val scaled = Bitmap.createScaledBitmap(square, edge, edge, true)
            if (scaled !== square) square.recycle()
            scaled
        }
    }

    /** Rotation/flip for an EXIF orientation tag, or [source] if none is needed. */
    private fun applyOrientation(source: Bitmap, orientation: Int): Bitmap {
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
            else -> return source
        }
        return Bitmap.createBitmap(source, 0, 0, source.width, source.height, matrix, true)
    }
}
