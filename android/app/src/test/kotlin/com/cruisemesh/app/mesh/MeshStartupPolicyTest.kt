package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MeshStartupPolicyTest {
    @Test
    fun `boot starts when enabled permissions remain granted and the user did not stop`() {
        assertTrue(
            shouldStartMeshAfterBoot(
                autoStartEnabled = true,
                meshEnabled = true,
                permissionsGranted = true,
            ),
        )
    }

    @Test
    fun `explicit notification stop suppresses boot startup`() {
        assertFalse(
            shouldStartMeshAfterBoot(
                autoStartEnabled = true,
                meshEnabled = false,
                permissionsGranted = true,
            ),
        )
    }

    @Test
    fun `disabled preference or revoked permissions suppresses boot startup`() {
        assertFalse(
            shouldStartMeshAfterBoot(
                autoStartEnabled = false,
                meshEnabled = true,
                permissionsGranted = true,
            ),
        )
        assertFalse(
            shouldStartMeshAfterBoot(
                autoStartEnabled = true,
                meshEnabled = true,
                permissionsGranted = false,
            ),
        )
    }

    @Test
    fun `app open respects durable mesh toggle`() {
        assertTrue(
            shouldStartMeshOnAppOpen(
                meshEnabled = true,
                permissionsGranted = true,
                runtimeStopped = true,
            ),
        )
        assertFalse(
            shouldStartMeshOnAppOpen(
                meshEnabled = false,
                permissionsGranted = true,
                runtimeStopped = true,
            ),
        )
    }

    @Test
    fun `notification permission is not required for mesh operation`() {
        val required = MeshService.requiredPermissions().toSet()

        assertTrue(required.contains(android.Manifest.permission.BLUETOOTH_SCAN))
        assertTrue(required.contains(android.Manifest.permission.BLUETOOTH_ADVERTISE))
        assertTrue(required.contains(android.Manifest.permission.BLUETOOTH_CONNECT))
        assertFalse(required.contains(android.Manifest.permission.POST_NOTIFICATIONS))
    }
}
