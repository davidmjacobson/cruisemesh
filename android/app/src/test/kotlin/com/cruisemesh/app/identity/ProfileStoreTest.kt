package com.cruisemesh.app.identity

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class ProfileStoreTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val preferences = context.getSharedPreferences("cruisemesh_profile", Context.MODE_PRIVATE)

    @Before
    fun setUp() {
        preferences.edit().clear().commit()
    }

    @After
    fun tearDown() {
        preferences.edit().clear().commit()
    }

    @Test
    fun `blank edit keeps the last real name`() {
        assertTrue(ProfileStore.saveDisplayName(context, "  Maya  ", durable = true))
        assertFalse(ProfileStore.saveDisplayName(context, "  ", durable = true))

        assertEquals("Maya", ProfileStore.loadStoredDisplayName(context))
        assertEquals("Maya", ProfileStore.loadDisplayName(context))
    }

    @Test
    fun `missing legacy name uses the defined fallback outside onboarding`() {
        assertEquals("", ProfileStore.loadStoredDisplayName(context))
        assertEquals("CruiseMesh user", ProfileStore.loadDisplayName(context))
    }
}
