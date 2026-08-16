package com.cruisemesh.app.sail

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreSailChecklistItemId
import uniffi.cruisemesh_core.CoreSailPermission
import uniffi.cruisemesh_core.coreSailChecklist

/**
 * The shell's half of the checklist: turning what this phone knows about
 * itself into the core's input record. The policy itself is the core's and is
 * tested there -- what these assert is that Android hands it the truth.
 */
class SailChecklistInputsTest {

    private fun state(
        contactCount: Int = 0,
        shorePassConfigured: Boolean = false,
        nearbyPermissionGranted: Boolean = false,
        notificationsPermissionGranted: Boolean = false,
        batteryOptimizationExempt: Boolean = false,
        offlineDeliverySeen: Boolean = false,
        backupCreated: Boolean = false,
    ) = SailChecklistDeviceState(
        contactCount = contactCount,
        shorePassConfigured = shorePassConfigured,
        nearbyPermissionGranted = nearbyPermissionGranted,
        notificationsPermissionGranted = notificationsPermissionGranted,
        batteryOptimizationExempt = batteryOptimizationExempt,
        offlineDeliverySeen = offlineDeliverySeen,
        backupCreated = backupCreated,
    )

    @Test
    fun everyDeviceFactReachesTheMatchingCoreInputField() {
        val input = SailChecklistInputs.coreInput(
            state(
                contactCount = 4,
                shorePassConfigured = true,
                nearbyPermissionGranted = true,
                notificationsPermissionGranted = false,
                batteryOptimizationExempt = true,
                offlineDeliverySeen = true,
                backupCreated = false,
            ),
        )

        assertEquals(4uL, input.contactCount)
        assertTrue(input.shorePassConfigured)
        assertTrue(input.bluetoothPermission)
        assertFalse(input.notificationsPermission)
        assertEquals(true, input.batteryOptimizationExempt)
        assertTrue(input.offlineDeliverySeen)
        assertFalse(input.backupCreated)
    }

    /**
     * Battery optimization is nullable only so iOS, which has no such setting,
     * can drop the row. On Android a missing exemption really does stop
     * delivery with the screen off, so it must arrive as a blocking `false`
     * rather than as "this platform doesn't have one".
     */
    @Test
    fun batteryOptimizationIsNeverAbsentOnAndroid() {
        val denied = SailChecklistInputs.coreInput(state(batteryOptimizationExempt = false))
        assertNotNull(denied.batteryOptimizationExempt)
        assertEquals(false, denied.batteryOptimizationExempt)

        val report = coreSailChecklist(denied)
        assertTrue(
            report.permissions.any { it.permission == CoreSailPermission.BATTERY_OPTIMIZATION },
        )
        assertFalse(
            report.items.single { it.id == CoreSailChecklistItemId.PERMISSIONS }.done,
        )
    }

    /** A count can only ever be a count; the core takes an unsigned one. */
    @Test
    fun anImpossibleNegativeContactCountBecomesZero() {
        assertEquals(0uL, SailChecklistInputs.coreInput(state(contactCount = -1)).contactCount)
    }

    /**
     * The end-to-end shape a family reaches on the way to the ship: family
     * added, grants given, one message sent with the internet off. The
     * optional steps are still open and must not hold "ready" back.
     */
    @Test
    fun requiredStepsAloneMakeTheFamilyReady() {
        val report = coreSailChecklist(
            SailChecklistInputs.coreInput(
                state(
                    contactCount = 2,
                    nearbyPermissionGranted = true,
                    notificationsPermissionGranted = true,
                    batteryOptimizationExempt = true,
                    offlineDeliverySeen = true,
                ),
            ),
        )

        assertTrue(report.ready)
        assertFalse(report.items.single { it.id == CoreSailChecklistItemId.SHORE_PASS }.done)
        assertFalse(report.items.single { it.id == CoreSailChecklistItemId.BACKUP }.done)
    }
}
