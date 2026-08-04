package com.cruisemesh.app.debug

import java.io.File
import java.util.zip.ZipFile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
}
