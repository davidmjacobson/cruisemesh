package com.cruisemesh.app.identity

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import java.io.File
import java.io.FileOutputStream
import kotlin.math.max

private const val PROFILE_DIR = "profile"
private const val PROFILE_PHOTO_FILE = "avatar.jpg"
private const val PROFILE_PHOTO_EDGE = 512

/** Persists the local profile photo shown in onboarding and the app chrome. */
object ProfilePhotoStore {

    fun loadAvatarPath(context: Context): String? =
        avatarFile(context).takeIf { it.isFile && it.length() > 0L }?.absolutePath

    fun clear(context: Context) {
        avatarFile(context).delete()
    }

    fun saveFromUri(context: Context, uri: Uri): String? {
        val bitmap = decodeSampledBitmap(context, uri) ?: return null
        return saveBitmap(context, bitmap)
    }

    private fun saveBitmap(context: Context, bitmap: Bitmap): String? =
        runCatching {
            val normalized = bitmap.centerCrop(PROFILE_PHOTO_EDGE)
            val file = avatarFile(context)
            file.parentFile?.mkdirs()
            FileOutputStream(file).use { output ->
                check(normalized.compress(Bitmap.CompressFormat.JPEG, 90, output))
            }
            file.absolutePath
        }.getOrNull()

    private fun decodeSampledBitmap(context: Context, uri: Uri): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        context.contentResolver.openInputStream(uri)?.use { stream ->
            BitmapFactory.decodeStream(stream, null, bounds)
        } ?: return null

        val maxDimension = max(bounds.outWidth, bounds.outHeight)
        if (maxDimension <= 0) return null

        var sampleSize = 1
        while (maxDimension / sampleSize > PROFILE_PHOTO_EDGE * 2) {
            sampleSize *= 2
        }

        val decodeOptions = BitmapFactory.Options().apply { inSampleSize = sampleSize }
        return context.contentResolver.openInputStream(uri)?.use { stream ->
            BitmapFactory.decodeStream(stream, null, decodeOptions)
        }
    }

    private fun avatarFile(context: Context): File =
        context.applicationContext.filesDir.resolve(PROFILE_DIR).resolve(PROFILE_PHOTO_FILE)

    private fun Bitmap.centerCrop(edge: Int): Bitmap {
        val squareEdge = minOf(width, height)
        val x = (width - squareEdge) / 2
        val y = (height - squareEdge) / 2
        val square = Bitmap.createBitmap(this, x, y, squareEdge, squareEdge)
        return Bitmap.createScaledBitmap(square, edge, edge, true)
    }
}
