package com.cruisemesh.app.debug

import org.junit.Assert.assertTrue
import org.junit.Test

class EnvironmentSnapshotTest {

    private fun env(
        powerSaveMode: Boolean = false,
        ignoringBatteryOptimizations: Boolean = true,
        standbyBucket: String = "active",
        thermalStatus: String = "none",
        network: String = "wifi",
        notificationsEnabled: Boolean = true,
        deniedPermissions: List<String> = emptyList(),
        freeDiskBytes: Long = 8L * 1024 * 1024 * 1024,
    ) = EnvironmentSnapshot.Environment(
        powerSaveMode,
        ignoringBatteryOptimizations,
        standbyBucket,
        thermalStatus,
        network,
        notificationsEnabled,
        deniedPermissions,
        freeDiskBytes,
    )

    @Test
    fun `a healthy device lists no denied permissions`() {
        val out = EnvironmentSnapshot.format(env())
        assertTrue(out, !out.contains("DENIED"))
        assertTrue(out, out.contains("powerSave=false"))
        assertTrue(out, out.contains("notifications=enabled"))
    }

    @Test
    fun `battery optimization is reported the way a reader thinks about it`() {
        // The platform API is phrased as "ignoring optimizations"; the log says
        // whether the app IS optimized, which is the condition that hurts.
        assertTrue(
            EnvironmentSnapshot.format(env(ignoringBatteryOptimizations = false))
                .contains("batteryOptimized=true"),
        )
        assertTrue(
            EnvironmentSnapshot.format(env(ignoringBatteryOptimizations = true))
                .contains("batteryOptimized=false"),
        )
    }

    @Test
    fun `denied permissions are shouted, not buried`() {
        val out = EnvironmentSnapshot.format(
            env(deniedPermissions = listOf("BLUETOOTH_SCAN", "POST_NOTIFICATIONS")),
        )
        assertTrue(out, out.contains("DENIED=BLUETOOTH_SCAN+POST_NOTIFICATIONS"))
    }

    @Test
    fun `disabled notifications are shouted too`() {
        val out = EnvironmentSnapshot.format(env(notificationsEnabled = false))
        assertTrue(out, out.contains("notifications=DISABLED"))
    }

    @Test
    fun `free disk is reported in megabytes`() {
        val out = EnvironmentSnapshot.format(env(freeDiskBytes = 512L * 1024 * 1024))
        assertTrue(out, out.contains("freeDisk=512MB"))
    }

    @Test
    fun `a restricted standby bucket survives into the line`() {
        val out = EnvironmentSnapshot.format(env(standbyBucket = "RESTRICTED"))
        assertTrue(out, out.contains("standbyBucket=RESTRICTED"))
    }

    @Test
    fun `a vpn on a metered link is visible`() {
        val out = EnvironmentSnapshot.format(env(network = "wifi+VPN,metered"))
        assertTrue(out, out.contains("network=wifi+VPN,metered"))
    }

    @Test
    fun `the line is one line`() {
        assertTrue(EnvironmentSnapshot.format(env()).trim().lineSequence().count() == 1)
    }
}
