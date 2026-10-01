package com.cruisemesh.app.debug

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * The captured file is what every export path hands out, so the property
 * pinned here is that the sink it is written through redacts -- not that the
 * core scanner works, which `core/src/log_redaction.rs` owns and covers in far
 * more detail.
 */
class DebugFileLogRedactionTest {

    @get:Rule
    val temp = TemporaryFolder()

    private val salt = "0123456789abcdef0123456789abcdef"

    @Test
    fun `a wi-fi address written to the log is replaced and the port survives`() {
        val file = temp.newFile("cruisemesh-log.txt")

        DebugFileLog.appendRedacted(file, salt, "BLE introduced LAN peer at 192.168.1.42:7777\n")

        val written = file.readText()
        assertFalse(written, written.contains("192.168.1.42"))
        assertTrue(written, written.trimEnd().endsWith(":7777"))
    }

    @Test
    fun `a bluetooth device address and a contact id are replaced`() {
        val file = temp.newFile("cruisemesh-log.txt")
        val id = "9f".repeat(32)

        DebugFileLog.appendRedacted(file, salt, "tearDownLink: AA:BB:CC:DD:EE:FF (idle)\n")
        DebugFileLog.appendRedacted(file, salt, "HELLO from userId=$id\n")

        val written = file.readText()
        assertFalse(written, written.contains("AA:BB:CC:DD:EE:FF"))
        assertFalse(written, written.contains(id))
        assertTrue(written, written.contains("device-"))
        assertTrue(written, written.contains("id-"))
    }

    /** Two lines about one peer still have to read as one peer. */
    @Test
    fun `the same peer keeps the same stand-in across lines`() {
        val file = temp.newFile("cruisemesh-log.txt")

        DebugFileLog.appendRedacted(file, salt, "connected 10.0.0.7:7777\n")
        DebugFileLog.appendRedacted(file, salt, "closed 10.0.0.7:7777\n")

        val lines = file.readLines()
        assertEquals(
            lines[0].removePrefix("connected "),
            lines[1].removePrefix("closed "),
        )
    }

    @Test
    fun `a line with nothing sensitive in it is written through untouched`() {
        val file = temp.newFile("cruisemesh-log.txt")
        val line = "08-27 14:23:11.123  4021  4099 I MeshService: " +
            "Relay sync complete: configs=2 net=wifi reason=periodic in 1234ms\n"

        DebugFileLog.appendRedacted(file, salt, line)

        assertEquals(line, file.readText())
    }

    /**
     * An update lands on phones that already hold an unredacted capture. The
     * diagnostics note would describe that file wrongly, and deleting a
     * tester's log to fix it would be worse than rewriting it.
     */
    @Test
    fun `an already-captured log is repaired in place`() {
        val file = temp.newFile("cruisemesh-log.txt")
        file.writeText("peer 192.168.1.42:7777\nnothing to see\n")

        assertTrue(DebugFileLog.redactInPlace(file, salt))

        val written = file.readText()
        assertFalse(written, written.contains("192.168.1.42"))
        assertTrue(written, written.contains("nothing to see"))
        assertFalse(
            "the staging copy must not survive",
            temp.root.walkTopDown().any { it.name.endsWith(".redacting") },
        )
    }
}
