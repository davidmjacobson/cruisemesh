package com.cruisemesh.app.friending

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class AirplaneDemoHintStoreTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val preferences = context.getSharedPreferences("cruisemesh_hints", Context.MODE_PRIVATE)

    @Before
    fun setUp() {
        preferences.edit().clear().commit()
    }

    @After
    fun tearDown() {
        preferences.edit().clear().commit()
    }

    @Test
    fun `offered on a fresh install`() {
        assertTrue(AirplaneDemoHintStore.shouldShow(context))
    }

    @Test
    fun `never offered twice`() {
        AirplaneDemoHintStore.markShown(context)
        assertFalse(AirplaneDemoHintStore.shouldShow(context))
    }

    @Test
    fun `marking twice is harmless`() {
        AirplaneDemoHintStore.markShown(context)
        AirplaneDemoHintStore.markShown(context)
        assertFalse(AirplaneDemoHintStore.shouldShow(context))
    }
}
