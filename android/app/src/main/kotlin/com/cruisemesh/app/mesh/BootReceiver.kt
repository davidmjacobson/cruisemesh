package com.cruisemesh.app.mesh

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.content.ContextCompat
import com.cruisemesh.app.identity.TermsAcceptanceStore

private const val TAG = "BootReceiver"

/** Restores the connected-device foreground service after an opted-in device reboot. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        if (!TermsAcceptanceStore.isCurrentVersionAccepted(context)) {
            Log.i(TAG, "Skipping mesh startup after boot until the current Terms of Use are accepted")
            return
        }

        val permissionsGranted = MeshService.requiredPermissions().all {
            ContextCompat.checkSelfPermission(context, it) == android.content.pm.PackageManager.PERMISSION_GRANTED
        }
        if (!shouldStartMeshAfterBoot(
                autoStartEnabled = MeshStartupPreferences.isAutoStartEnabled(context),
                explicitlyStopped = MeshStartupPreferences.wasExplicitlyStopped(context),
                permissionsGranted = permissionsGranted,
            )
        ) {
            Log.i(TAG, "Skipping mesh startup after boot because startup policy did not allow it")
            return
        }

        try {
            ContextCompat.startForegroundService(context, Intent(context, MeshService::class.java))
        } catch (e: RuntimeException) {
            Log.w(TAG, "Android did not allow mesh startup after boot", e)
        }
    }
}
