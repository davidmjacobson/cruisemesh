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
        AirplaneDemoHintStore.refresh(context)
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
        AirplaneDemoHintStore.dismiss(context)
        assertFalse(AirplaneDemoHintStore.shouldShow(context))
    }

    @Test
    fun `dismissing twice is harmless`() {
        AirplaneDemoHintStore.dismiss(context)
        AirplaneDemoHintStore.dismiss(context)
        assertFalse(AirplaneDemoHintStore.shouldShow(context))
    }

    /**
     * Both surfaces that show the hint read this flow, so a dismissal on either
     * one has to reach the other without a reload.
     */
    @Test
    fun `the flow follows the saved answer`() {
        assertTrue(AirplaneDemoHintStore.showHint.value)
        AirplaneDemoHintStore.dismiss(context)
        assertFalse(AirplaneDemoHintStore.showHint.value)
    }

    /** A restart re-reads the flag rather than starting the hint over. */
    @Test
    fun `refresh reloads a dismissal from a previous launch`() {
        AirplaneDemoHintStore.dismiss(context)
        AirplaneDemoHintStore.refresh(context)
        assertFalse(AirplaneDemoHintStore.showHint.value)
    }
}
