package com.cruisemesh.app.ui

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.cruisemesh.app.debug.DebugFileLog
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The unlock flag has to survive the app being killed: the two halves of a
 * canary run are separated by hours, and a tester who had to re-tap after every
 * restart would give up on the run instead.
 */
@RunWith(RobolectricTestRunner::class)
class InternalToolsUnlockStoreTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val preferences =
        context.getSharedPreferences("cruisemesh_internal_tools", Context.MODE_PRIVATE)

    @Before
    fun setUp() {
        preferences.edit().clear().commit()
    }

    @After
    fun tearDown() {
        preferences.edit().clear().commit()
    }

    @Test
    fun `locked on a fresh install`() {
        assertFalse(InternalToolsUnlockStore.isUnlocked(context))
    }

    @Test
    fun `unlocking persists`() {
        InternalToolsUnlockStore.setUnlocked(context, true)
        assertTrue(InternalToolsUnlockStore.isUnlocked(context))
    }

    @Test
    fun `locking again clears it`() {
        InternalToolsUnlockStore.setUnlocked(context, true)
        InternalToolsUnlockStore.setUnlocked(context, false)
        assertFalse(InternalToolsUnlockStore.isUnlocked(context))
    }

    @Test
    fun `unlocking shows the entry and the warning follows the build type`() {
        val debuggable = DebugFileLog.isDebuggableBuild(context)

        // Locked: only a debuggable build shows the entry, and nothing warns.
        assertEquals(debuggable, internalToolsVisible(context))
        assertFalse(internalToolsUnlockedOnRelease(context))

        InternalToolsUnlockStore.setUnlocked(context, true)

        // Unlocked: the entry is there on any build, and the warning appears
        // exactly where it is meant to -- on a release build, never on a
        // developer's own.
        assertTrue(internalToolsVisible(context))
        assertEquals(!debuggable, internalToolsUnlockedOnRelease(context))
    }
}
