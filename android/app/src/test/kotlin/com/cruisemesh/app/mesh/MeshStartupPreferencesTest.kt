package com.cruisemesh.app.mesh

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MeshStartupPreferencesTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val preferences by lazy {
        context.getSharedPreferences("cruisemesh_mesh_startup", Context.MODE_PRIVATE)
    }

    @Before
    fun clearBefore() {
        preferences.edit().clear().commit()
    }

    @After
    fun clearAfter() {
        preferences.edit().clear().commit()
    }

    @Test
    fun `mesh intent defaults on and persists both choices`() {
        assertTrue(MeshStartupPreferences.isMeshEnabled(context))

        MeshStartupPreferences.setMeshEnabled(context, false)
        assertFalse(MeshStartupPreferences.isMeshEnabled(context))

        MeshStartupPreferences.setMeshEnabled(context, true)
        assertTrue(MeshStartupPreferences.isMeshEnabled(context))
    }

    @Test
    fun `legacy explicit stop migrates to disabled intent`() {
        preferences.edit().putBoolean("explicitly_stopped", true).commit()

        assertFalse(MeshStartupPreferences.isMeshEnabled(context))

        MeshStartupPreferences.setMeshEnabled(context, true)
        assertTrue(MeshStartupPreferences.isMeshEnabled(context))
        assertFalse(preferences.contains("explicitly_stopped"))
    }
}
