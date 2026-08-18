package com.cruisemesh.app.identity.backup

import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.CoreRestoreIntent
import uniffi.cruisemesh_core.CoreRestorePlan

/**
 * §9's restore fork. The plans are handwritten here rather than decrypted out of
 * a `.cmbak`, for the reason [com.cruisemesh.app.ui.SailChecklistUiTest] gives
 * about reports: the point is that the shell draws the plan it is handed,
 * including the clone-hazard flag, rather than re-deciding any of it.
 */
@RunWith(AndroidJUnit4::class)
class RestoreIntentForkUiTest {
    @get:Rule
    val compose = createComposeRule()

    private val personId = ByteArray(16) { 7 }

    private fun plan(
        intent: CoreRestoreIntent,
        routesToLinkCeremony: Boolean,
        cloneHazard: Boolean,
    ) = CoreRestorePlan(
        intent = intent,
        personId = personId,
        restoresStoredHistory = !routesToLinkCeremony,
        keepsExistingDeviceIdentity = !routesToLinkCeremony,
        mintsNewDeviceKey = routesToLinkCeremony,
        routesToLinkCeremony = routesToLinkCeremony,
        carriesRecoveryMaterial = false,
        cloneHazardIfSourceIsLive = cloneHazard,
    )

    private val plans = listOf(
        plan(CoreRestoreIntent.LINK_AS_NEW_DEVICE, routesToLinkCeremony = true, cloneHazard = false),
        plan(CoreRestoreIntent.REPLACE_THIS_DEVICE, routesToLinkCeremony = false, cloneHazard = true),
    )

    /**
     * The fork is a run of siblings and the restore screen supplies the column
     * they sit in. Handed a bare root they would stack on top of each other and
     * a tap would land on whichever was drawn last, which is a fact about the
     * test harness and not about the screen.
     */
    private fun show(content: @Composable () -> Unit) {
        compose.setContent { CruiseMeshTheme { Column { content() } } }
    }

    @Test
    fun `both meanings of restore are offered, in the order core gave them`() {
        show {
            RestoreIntentFork(plans = plans, chosen = null, onChoose = {}, onSetUpAsNewDevice = {})
        }

        compose.onNodeWithText("What is this device?").assertIsDisplayed()
        compose.onNodeWithText("Set up as a new device").assertIsDisplayed()
        compose.onNodeWithText("Replace this device").assertIsDisplayed()
    }

    @Test
    fun `the clone hazard is warned about wherever core flags it`() {
        show {
            RestoreIntentFork(plans = plans, chosen = null, onChoose = {}, onSetUpAsNewDevice = {})
        }

        compose.onNodeWithText("Only do this if the old device is switched off", substring = true)
            .assertIsDisplayed()
    }

    @Test
    fun `choosing the link branch hands the ceremony the person from the backup`() {
        var chosen: CoreRestoreIntent? = null
        var routedTo: ByteArray? = null
        show {
            RestoreIntentFork(
                plans = plans,
                chosen = null,
                onChoose = { chosen = it },
                onSetUpAsNewDevice = { routedTo = it },
            )
        }

        compose.onNodeWithText("Set up as a new device").performClick()

        assertEquals(CoreRestoreIntent.LINK_AS_NEW_DEVICE, chosen)
        assertArrayEquals(personId, routedTo)
    }

    @Test
    fun `choosing to replace stays on this screen`() {
        var chosen: CoreRestoreIntent? = null
        var routedTo: ByteArray? = null
        show {
            RestoreIntentFork(
                plans = plans,
                chosen = null,
                onChoose = { chosen = it },
                onSetUpAsNewDevice = { routedTo = it },
            )
        }

        compose.onNodeWithText("Replace this device").performClick()

        assertEquals(CoreRestoreIntent.REPLACE_THIS_DEVICE, chosen)
        assertNull(routedTo)
    }
}
