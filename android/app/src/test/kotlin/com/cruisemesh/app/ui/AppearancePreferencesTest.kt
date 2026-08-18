package com.cruisemesh.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppearancePreferencesTest {
    @Test
    fun `missing and unrecognized values follow the system`() {
        assertEquals(AppearancePreference.SYSTEM, AppearancePreference.fromStoredValue(null))
        assertEquals(AppearancePreference.SYSTEM, AppearancePreference.fromStoredValue("sepia"))
    }

    @Test
    fun `stored values round trip`() {
        AppearancePreference.entries.forEach { preference ->
            assertEquals(
                preference,
                AppearancePreference.fromStoredValue(preference.storageValue),
            )
        }
    }

    @Test
    fun `theme choices resolve against the system setting`() {
        assertFalse(AppearancePreference.SYSTEM.resolvesDark(systemIsDark = false))
        assertTrue(AppearancePreference.SYSTEM.resolvesDark(systemIsDark = true))
        assertFalse(AppearancePreference.LIGHT.resolvesDark(systemIsDark = true))
        assertTrue(AppearancePreference.DARK.resolvesDark(systemIsDark = false))
    }
}
