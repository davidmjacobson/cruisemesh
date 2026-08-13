package com.cruisemesh.app.debug

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DiagnosticsShareHandoffTest {

    @After
    fun tearDown() {
        DiagnosticsShareHandoff.reset()
    }

    @Test
    fun `opening the armed archive is the share confirmation`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")
        assertTrue(DiagnosticsShareHandoff.takeIfConsumed())
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `a different file is not a diagnostics handoff`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.onOpened("content://app/photo.jpg")
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `picking Drive is not enough — the file has to be read`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `the listener fires only for the armed archive`() {
        val seen = mutableListOf<String>()
        DiagnosticsShareHandoff.setListener { seen += "opened" }
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")

        DiagnosticsShareHandoff.onOpened("content://app/other")
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")

        assertEquals(listOf("opened"), seen)
    }
}
