package com.cruisemesh.app.mesh

import android.annotation.SuppressLint
import android.content.Context

/** Durable user intent for running the mesh and restoring it after a reboot. */
object MeshStartupPreferences {
    private const val PREFS_NAME = "cruisemesh_mesh_startup"
    private const val KEY_AUTO_START = "auto_start"
    private const val KEY_MESH_ENABLED = "mesh_enabled"
    // Read only for migration from builds where a notification stop lasted
    // until the app was opened again instead of being a real on/off choice.
    private const val KEY_EXPLICITLY_STOPPED = "explicitly_stopped"

    fun isAutoStartEnabled(context: Context): Boolean =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(KEY_AUTO_START, true)

    fun setAutoStartEnabled(context: Context, enabled: Boolean) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(KEY_AUTO_START, enabled)
            .apply()
    }

    fun isMeshEnabled(context: Context): Boolean {
        val preferences = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        return if (preferences.contains(KEY_MESH_ENABLED)) {
            preferences.getBoolean(KEY_MESH_ENABLED, true)
        } else {
            !preferences.getBoolean(KEY_EXPLICITLY_STOPPED, false)
        }
    }

    @SuppressLint("ApplySharedPref")
    fun setMeshEnabled(context: Context, enabled: Boolean) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(KEY_MESH_ENABLED, enabled)
            .remove(KEY_EXPLICITLY_STOPPED)
            // Persist before starting/stopping the foreground service so a
            // process eviction cannot erase the user's explicit choice.
            .commit()
    }
}

internal fun shouldStartMeshAfterBoot(
    autoStartEnabled: Boolean,
    meshEnabled: Boolean,
    permissionsGranted: Boolean,
): Boolean = autoStartEnabled && meshEnabled && permissionsGranted

internal fun shouldStartMeshOnAppOpen(
    meshEnabled: Boolean,
    permissionsGranted: Boolean,
    runtimeStopped: Boolean,
): Boolean = meshEnabled && permissionsGranted && runtimeStopped
