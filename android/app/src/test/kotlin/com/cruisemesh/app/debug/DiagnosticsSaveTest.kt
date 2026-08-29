package com.cruisemesh.app.debug

import android.content.Context
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

/**
 * The copy into the document the picker returned.
 *
 * The whole point of "Save to device" is that the file on the far side is the
 * same file the share sheet would have sent, so the byte comparison is the
 * test that matters -- a truncated or short-copied archive would still look
 * like a success to the person who tapped the button.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class DiagnosticsSaveTest {

    @get:Rule val temp = TemporaryFolder()

    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    @Test
    fun `the saved document gets the source bytes, whole`() {
        // Larger than one copy buffer, and binary, because a zip is: a copy
        // that stopped at the first chunk or mangled a byte would pass a
        // small-ASCII test.
        val bytes = ByteArray(64 * 1024) { (it * 31 % 251).toByte() }
        val source = temp.newFile("diagnostics.zip").apply { writeBytes(bytes) }
        val target = Uri.parse("content://test.documents/saved")
        val written = ByteArrayOutputStream()
        shadowOf(context.contentResolver).registerOutputStream(target, written)

        assertTrue(DiagnosticsShare.writeTo(context, source, target))

        assertArrayEquals(bytes, written.toByteArray())
    }

    /**
     * A picked document whose provider is gone -- an unmounted USB drive, an
     * app uninstalled while the picker was open. Reporting a save that did not
     * happen is the one outcome worse than the error.
     */
    @Test
    fun `an unwritable target reports failure instead of a save`() {
        val source = temp.newFile("diagnostics.zip").apply { writeBytes(byteArrayOf(1, 2, 3)) }

        assertFalse(
            DiagnosticsShare.writeTo(context, source, Uri.parse("content://nobody.at.home/x")),
        )
    }
}
