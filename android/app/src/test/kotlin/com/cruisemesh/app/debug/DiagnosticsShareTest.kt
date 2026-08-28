package com.cruisemesh.app.debug

import java.io.File
import java.time.LocalDateTime
import java.util.zip.ZipFile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class DiagnosticsShareTest {

    @get:Rule
    val temp = TemporaryFolder()

    /**
     * The whole point of the zip: every captured file has to be inside it.
     * Sharing them as separate attachments let receiving apps keep the first
     * and drop the rest, silently.
     */
    @Test
    fun `archive holds every captured file, contents intact`() {
        val log = temp.newFile("cruisemesh-log.txt").apply { writeText("radio narrative\n") }
        val csv = temp.newFile("cruisemesh-field-metrics.csv").apply { writeText("a,b\n1,2\n") }
        val dest = File(temp.newFolder("out"), "diagnostics.zip")

        assertEquals(dest, DiagnosticsShare.writeArchive(listOf(log, csv), dest))

        ZipFile(dest).use { zip ->
            val entries = zip.entries().toList().map { it.name }
            assertEquals(listOf("cruisemesh-log.txt", "cruisemesh-field-metrics.csv"), entries)
            assertEquals(
                "radio narrative\n",
                zip.getInputStream(zip.getEntry("cruisemesh-log.txt")).reader().readText(),
            )
            assertEquals(
                "a,b\n1,2\n",
                zip.getInputStream(zip.getEntry("cruisemesh-field-metrics.csv")).reader().readText(),
            )
        }
    }

    /** Entry names must not carry the device's directory layout. */
    @Test
    fun `entries are named by file, not by path`() {
        val nested = File(temp.newFolder("logs"), "cruisemesh-log.txt").apply { writeText("x") }
        val dest = File(temp.root, "diagnostics.zip")

        DiagnosticsShare.writeArchive(listOf(nested), dest)

        ZipFile(dest).use { zip ->
            assertEquals(listOf("cruisemesh-log.txt"), zip.entries().toList().map { it.name })
        }
    }

    /**
     * A half-written zip would share as a plausible attachment and fail to open
     * on the far side, so a failed write must leave nothing behind.
     */
    @Test
    fun `a failed write leaves no partial archive`() {
        val good = temp.newFile("cruisemesh-log.txt").apply { writeText("kept") }
        val missing = File(temp.root, "never-written.csv")
        val dest = File(temp.root, "diagnostics.zip")

        assertNull(DiagnosticsShare.writeArchive(listOf(good, missing), dest))
        assertFalse(dest.exists())
    }

    @Test
    fun `an empty capture set still produces a readable archive`() {
        val dest = File(temp.root, "diagnostics.zip")

        assertTrue(DiagnosticsShare.writeArchive(emptyList(), dest) != null)
        ZipFile(dest).use { zip -> assertTrue(zip.entries().toList().isEmpty()) }
    }

    /**
     * The name offered to the document picker. Spaces are the thing to avoid:
     * a saved file gets typed into a shell or pasted into a mail client, and
     * both of those break a name at the first one.
     */
    @Test
    fun `the suggested save name is stamped, unspaced, and keeps the extension`() {
        val name = DiagnosticsShare.saveFileName(
            "cruisemesh-diagnostics-2026-08-27.zip",
            LocalDateTime.of(2026, 8, 27, 14, 32, 5),
        )

        assertEquals("cruisemesh-diagnostics-20260827-143205.zip", name)
        assertFalse(name.contains(' '))
    }

    /**
     * A full device makes zipping fail and the log goes alone; proposing that
     * as a .zip would produce a file no one can open.
     */
    @Test
    fun `the suggested save name follows the payload, not the zip`() {
        assertEquals(
            "cruisemesh-diagnostics-20260101-000000.txt",
            DiagnosticsShare.saveFileName(
                "cruisemesh-log.txt",
                LocalDateTime.of(2026, 1, 1, 0, 0, 0),
            ),
        )
    }

    /** An extensionless payload must not be proposed with a trailing dot. */
    @Test
    fun `a payload with no extension yields a name with no extension`() {
        assertEquals(
            "cruisemesh-diagnostics-20260101-000000",
            DiagnosticsShare.saveFileName("capture", LocalDateTime.of(2026, 1, 1, 0, 0, 0)),
        )
    }

    /**
     * Two saves in one afternoon is the normal case while working a problem.
     * Second-level stamping is what keeps the picker from silently renaming
     * the newer one and the older one getting sent instead.
     */
    @Test
    fun `two saves in the same day get different names`() {
        val morning = DiagnosticsShare.saveFileName(
            "diagnostics.zip",
            LocalDateTime.of(2026, 8, 27, 9, 15, 0),
        )
        val evening = DiagnosticsShare.saveFileName(
            "diagnostics.zip",
            LocalDateTime.of(2026, 8, 27, 21, 15, 0),
        )

        assertNotEquals(morning, evening)
    }
}
