package com.cruisemesh.app.mesh

import android.annotation.SuppressLint
import android.content.Context

/** Durable user intent that controls whether the foreground mesh may return after a reboot. */
object MeshStartupPreferences {
    private const val PREFS_NAME = "cruisemesh_mesh_startup"
    private const val KEY_AUTO_START = "auto_start"
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

    fun wasExplicitlyStopped(context: Context): Boolean =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(KEY_EXPLICITLY_STOPPED, false)

    @SuppressLint("ApplySharedPref")
    fun markExplicitlyStopped(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(KEY_EXPLICITLY_STOPPED, true)
            // Persist before stopping the foreground service so a process
            // eviction cannot erase the user's explicit choice.
            .commit()
    }

    /** Opening or manually starting the app begins a new session after an explicit notification stop. */
    fun clearExplicitStop(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .remove(KEY_EXPLICITLY_STOPPED)
            .apply()
    }
}

internal fun shouldStartMeshAfterBoot(
    autoStartEnabled: Boolean,
    explicitlyStopped: Boolean,
    permissionsGranted: Boolean,
): Boolean = autoStartEnabled && !explicitlyStopped && permissionsGranted
