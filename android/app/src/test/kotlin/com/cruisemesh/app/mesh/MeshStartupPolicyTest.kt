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
                explicitlyStopped = false,
                permissionsGranted = true,
            ),
        )
    }

    @Test
    fun `explicit notification stop suppresses boot startup`() {
        assertFalse(
            shouldStartMeshAfterBoot(
                autoStartEnabled = true,
                explicitlyStopped = true,
                permissionsGranted = true,
            ),
        )
    }

    @Test
    fun `disabled preference or revoked permissions suppresses boot startup`() {
        assertFalse(
            shouldStartMeshAfterBoot(
                autoStartEnabled = false,
                explicitlyStopped = false,
                permissionsGranted = true,
            ),
        )
        assertFalse(
            shouldStartMeshAfterBoot(
                autoStartEnabled = true,
                explicitlyStopped = false,
                permissionsGranted = false,
            ),
        )
    }
}
