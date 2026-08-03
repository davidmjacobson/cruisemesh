package com.cruisemesh.app.debug

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ProcessExitHistoryTest {

    private fun exit(
        timestampMs: Long = 1_785_786_345_276L,
        reasonLabel: String = "CRASH (unhandled exception)",
        description: String? = null,
        trace: String? = null,
    ) = ProcessExitHistory.Exit(timestampMs, reasonLabel, description, trace)

    @Test
    fun `a first-ever launch adds nothing`() {
        assertEquals("", ProcessExitHistory.format(emptyList()))
    }

    @Test
    fun `an exit reports its reason`() {
        val out = ProcessExitHistory.format(listOf(exit(reasonLabel = "ANR (app not responding)")))
        assertTrue(out, out.contains("ANR (app not responding)"))
        assertTrue(out, out.contains("previous process exits"))
    }

    @Test
    fun `the OS description rides along when present`() {
        val out = ProcessExitHistory.format(listOf(exit(description = "user request after error")))
        assertTrue(out, out.contains("user request after error"))
    }

    @Test
    fun `a blank description does not leave a dangling separator`() {
        val out = ProcessExitHistory.format(listOf(exit(description = "   ")))
        assertFalseContains(out, "—")
    }

    @Test
    fun `a trace is fenced so it cannot be mistaken for log lines`() {
        val out = ProcessExitHistory.format(listOf(exit(trace = "frame one\nframe two")))
        assertTrue(out, out.contains("--- trace ---"))
        assertTrue(out, out.contains("--- end trace ---"))
        assertTrue(out, out.contains("frame one"))
        assertTrue(out, out.contains("frame two"))
    }

    @Test
    fun `every exit in a crash loop is listed`() {
        val out = ProcessExitHistory.format(
            listOf(
                exit(timestampMs = 1_785_786_000_000L),
                exit(timestampMs = 1_785_785_000_000L),
                exit(timestampMs = 1_785_784_000_000L),
            ),
        )
        assertEquals(3, out.lineSequence().count { it.contains("CRASH (unhandled exception)") })
    }

    private fun assertFalseContains(haystack: String, needle: String) {
        assertTrue(haystack, !haystack.contains(needle))
    }
}
