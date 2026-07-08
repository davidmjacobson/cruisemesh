package com.cruisemesh.app.friending

import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer

/**
 * Decodes QR codes from a CameraX frame stream using ZXing. Frames without a
 * decodable code are the common case (most of the time nothing is pointed at
 * the camera yet), so [NotFoundException] is expected, not an error.
 */
class QrAnalyzer(private val onDecoded: (String) -> Unit) : ImageAnalysis.Analyzer {
    private val reader = MultiFormatReader().apply {
        setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE)))
    }

    override fun analyze(image: ImageProxy) {
        try {
            val luminance = extractLuminance(image)
            val source = PlanarYUVLuminanceSource(
                luminance, image.width, image.height, 0, 0, image.width, image.height, false,
            )
            val result = reader.decodeWithState(BinaryBitmap(HybridBinarizer(source)))
            onDecoded(result.text)
        } catch (_: NotFoundException) {
            // No QR code in this frame -- expected for most frames.
        } catch (_: Exception) {
            // Decode failed for this frame; try again on the next one.
        } finally {
            image.close()
        }
    }

    /** Copies the Y plane into a tightly packed buffer, respecting row stride padding. */
    private fun extractLuminance(image: ImageProxy): ByteArray {
        val plane = image.planes[0]
        val buffer = plane.buffer
        val rowStride = plane.rowStride
        val width = image.width
        val height = image.height

        if (rowStride == width) {
            val data = ByteArray(buffer.remaining())
            buffer.get(data)
            return data
        }

        val data = ByteArray(width * height)
        val row = ByteArray(rowStride)
        for (y in 0 until height) {
            buffer.position(y * rowStride)
            buffer.get(row, 0, minOf(rowStride, buffer.remaining()))
            System.arraycopy(row, 0, data, y * width, width)
        }
        return data
    }
}
