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
    fun `opening the armed archive after a target is chosen is the confirmation`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.markTargetChosen()
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")
        assertTrue(DiagnosticsShareHandoff.takeIfConsumed())
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `a different file is not a diagnostics handoff`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.markTargetChosen()
        DiagnosticsShareHandoff.onOpened("content://app/photo.jpg")
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `picking Drive is not enough — the file has to be read`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.markTargetChosen()
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `an open before a target is chosen is preview, not a share`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `backing out of the chooser disarms an unused share`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.cancelPending()
        DiagnosticsShareHandoff.markTargetChosen()
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")
        assertFalse(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `cancel leaves a share that was already read`() {
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.markTargetChosen()
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.cancelPending()
        assertTrue(DiagnosticsShareHandoff.takeIfConsumed())
    }

    @Test
    fun `write-only and own-app opens do not count`() {
        assertFalse(DiagnosticsShareHandoff.isCountableOpen("w", "com.other", "com.cruisemesh.app"))
        assertFalse(
            DiagnosticsShareHandoff.isCountableOpen("rw", "com.cruisemesh.app", "com.cruisemesh.app"),
        )
        assertFalse(
            DiagnosticsShareHandoff.isCountableOpen("r", "com.android.intentresolver", "com.cruisemesh.app"),
        )
        assertTrue(DiagnosticsShareHandoff.isCountableOpen("r", "com.google.android.apps.docs", "com.cruisemesh.app"))
        assertTrue(DiagnosticsShareHandoff.isCountableOpen("rw", "com.google.android.apps.docs", "com.cruisemesh.app"))
    }

    @Test
    fun `the listener fires only for the armed archive`() {
        val seen = mutableListOf<String>()
        DiagnosticsShareHandoff.setListener { seen += "opened" }
        DiagnosticsShareHandoff.expect("content://app/diagnostics.zip")
        DiagnosticsShareHandoff.markTargetChosen()

        DiagnosticsShareHandoff.onOpened("content://app/other")
        DiagnosticsShareHandoff.onOpened("content://app/diagnostics.zip")

        assertEquals(listOf("opened"), seen)
    }
}
