package com.cruisemesh.app.debug

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DebugFileLogTest {

    @Test
    fun `debuggable builds always capture`() {
        assertTrue(DebugFileLog.shouldCapture(debuggable = true, optedIn = false))
        assertTrue(DebugFileLog.shouldCapture(debuggable = true, optedIn = true))
    }

    @Test
    fun `release builds capture only on opt-in`() {
        assertFalse(DebugFileLog.shouldCapture(debuggable = false, optedIn = false))
        assertTrue(DebugFileLog.shouldCapture(debuggable = false, optedIn = true))
    }
}
