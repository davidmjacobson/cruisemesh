package com.cruisemesh.app.debug

import android.Manifest
import android.app.usage.UsageStatsManager
import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.PowerManager
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

/**
 * The device conditions that silently stop the mesh working.
 *
 * Every one of these can leave a log that looks like a healthy app doing
 * nothing at all: battery optimization puts the app in a restricted standby
 * bucket so background sync never runs; power-save mode and thermal throttling
 * squeeze the radios; a metered or unvalidated network suppresses relay sync; a
 * full-tunnel VPN takes the default route out from under relay binding; a
 * revoked BLUETOOTH_SCAN leaves discovery silently empty. None of it produces
 * an error line, so without a snapshot the reader is comparing an uneventful
 * log against a report that "it just stopped working".
 *
 * Captured once per capture session and written into the header. Metadata only:
 * transport *types*, never an SSID, and never an address.
 */
object EnvironmentSnapshot {
    /**
     * The fields worth writing down, free of Android types so [format] is
     * unit-testable on the JVM.
     */
    data class Environment(
        val powerSaveMode: Boolean,
        val ignoringBatteryOptimizations: Boolean,
        val standbyBucket: String,
        val thermalStatus: String,
        val network: String,
        val notificationsEnabled: Boolean,
        val deniedPermissions: List<String>,
        val freeDiskBytes: Long,
    )

    /** One line for the capture header. Deliberately terse and greppable. */
    fun format(env: Environment): String {
        val parts = mutableListOf(
            "powerSave=${env.powerSaveMode}",
            "batteryOptimized=${!env.ignoringBatteryOptimizations}",
            "standbyBucket=${env.standbyBucket}",
            "thermal=${env.thermalStatus}",
            "network=${env.network}",
            "notifications=${if (env.notificationsEnabled) "enabled" else "DISABLED"}",
            "freeDisk=${env.freeDiskBytes / 1_048_576}MB",
        )
        if (env.deniedPermissions.isNotEmpty()) {
            parts.add("DENIED=${env.deniedPermissions.joinToString("+")}")
        }
        return "  environment: ${parts.joinToString(" ")}\n"
    }

    /** Reads the current conditions. Every lookup is guarded: a diagnostics
     * line must never be the thing that crashes the app. */
    fun capture(context: Context): Environment {
        val power = context.getSystemService(Context.POWER_SERVICE) as? PowerManager
        return Environment(
            powerSaveMode = power?.isPowerSaveMode ?: false,
            ignoringBatteryOptimizations =
                runCatching { power?.isIgnoringBatteryOptimizations(context.packageName) }
                    .getOrNull() ?: false,
            standbyBucket = standbyBucket(context),
            thermalStatus = thermalStatus(power),
            network = network(context),
            notificationsEnabled =
                runCatching { NotificationManagerCompat.from(context).areNotificationsEnabled() }
                    .getOrNull() ?: false,
            deniedPermissions = deniedPermissions(context),
            freeDiskBytes = runCatching { context.filesDir.usableSpace }.getOrNull() ?: 0L,
        )
    }

    /**
     * How aggressively the OS is deferring this app's background work. RARE or
     * RESTRICTED is the difference between "the mesh is broken" and "Android
     * decided this app does not run".
     */
    private fun standbyBucket(context: Context): String {
        val usage = context.getSystemService(Context.USAGE_STATS_SERVICE) as? UsageStatsManager
            ?: return "unknown"
        return when (runCatching { usage.appStandbyBucket }.getOrNull()) {
            UsageStatsManager.STANDBY_BUCKET_ACTIVE -> "active"
            UsageStatsManager.STANDBY_BUCKET_WORKING_SET -> "workingSet"
            UsageStatsManager.STANDBY_BUCKET_FREQUENT -> "frequent"
            UsageStatsManager.STANDBY_BUCKET_RARE -> "RARE"
            UsageStatsManager.STANDBY_BUCKET_RESTRICTED -> "RESTRICTED"
            else -> "unknown"
        }
    }

    private fun thermalStatus(power: PowerManager?): String =
        when (runCatching { power?.currentThermalStatus }.getOrNull()) {
            PowerManager.THERMAL_STATUS_NONE -> "none"
            PowerManager.THERMAL_STATUS_LIGHT -> "light"
            PowerManager.THERMAL_STATUS_MODERATE -> "moderate"
            PowerManager.THERMAL_STATUS_SEVERE -> "severe"
            PowerManager.THERMAL_STATUS_CRITICAL -> "critical"
            PowerManager.THERMAL_STATUS_EMERGENCY -> "emergency"
            PowerManager.THERMAL_STATUS_SHUTDOWN -> "shutdown"
            else -> "unknown"
        }

    /**
     * Transport types and constraints, never an SSID. A VPN reports its own
     * transport alongside the real one, which is the fingerprint of the
     * always-on full-tunnel setup that has confounded relay debugging before.
     */
    private fun network(context: Context): String {
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return "unknown"
        val caps = runCatching { cm.getNetworkCapabilities(cm.activeNetwork) }.getOrNull()
            ?: return "none"
        val transports = mutableListOf<String>()
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) transports.add("wifi")
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) transports.add("cellular")
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) transports.add("ethernet")
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) transports.add("VPN")
        val flags = mutableListOf(transports.joinToString("+").ifEmpty { "no-transport" })
        if (!caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) {
            flags.add("unvalidated")
        }
        if (!caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)) {
            flags.add("metered")
        }
        return flags.joinToString(",")
    }

    /**
     * Runtime permissions the mesh needs that are not currently granted. Listed
     * by the short name; a denied BLUETOOTH_SCAN produces an empty-looking
     * discovery log that is otherwise indistinguishable from an empty room.
     *
     * Only permissions the manifest actually declares belong here. Checking one
     * the app never requests -- NEARBY_WIFI_DEVICES, say, which this app does
     * not need because LAN discovery goes through NSD -- would report DENIED on
     * every healthy device and train the reader to ignore the field.
     */
    private fun deniedPermissions(context: Context): List<String> = listOf(
        Manifest.permission.BLUETOOTH_SCAN,
        Manifest.permission.BLUETOOTH_ADVERTISE,
        Manifest.permission.BLUETOOTH_CONNECT,
        Manifest.permission.POST_NOTIFICATIONS,
    ).filter {
        runCatching {
            ContextCompat.checkSelfPermission(context, it) != PackageManager.PERMISSION_GRANTED
        }.getOrNull() ?: false
    }.map { it.substringAfterLast('.') }
}
