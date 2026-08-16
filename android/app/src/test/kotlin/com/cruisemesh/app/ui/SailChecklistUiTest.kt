package com.cruisemesh.app.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.CoreSailChecklistItem
import uniffi.cruisemesh_core.CoreSailChecklistItemId
import uniffi.cruisemesh_core.CoreSailChecklistReport
import uniffi.cruisemesh_core.CoreSailPermission
import uniffi.cruisemesh_core.CoreSailPermissionRow

/**
 * The checklist screen renders exactly what the core report says and nothing
 * of its own. The reports here are written by hand rather than computed: the
 * point is that a shell handed a given verdict draws that verdict, including
 * verdicts the real policy would have to be coaxed into producing.
 */
@RunWith(AndroidJUnit4::class)
class SailChecklistUiTest {
    @get:Rule
    val compose = createComposeRule()

    private fun report(
        shorePassDone: Boolean = false,
        addFamilyDone: Boolean = false,
        permissionsDone: Boolean = false,
        offlineTestDone: Boolean = false,
        backupDone: Boolean = false,
        permissions: List<CoreSailPermissionRow> = listOf(
            CoreSailPermissionRow(CoreSailPermission.BLUETOOTH, granted = false),
            CoreSailPermissionRow(CoreSailPermission.NOTIFICATIONS, granted = false),
            CoreSailPermissionRow(CoreSailPermission.BATTERY_OPTIMIZATION, granted = false),
        ),
        ready: Boolean = false,
    ): CoreSailChecklistReport {
        val items = listOf(
            CoreSailChecklistItem(CoreSailChecklistItemId.SHORE_PASS, false, shorePassDone),
            CoreSailChecklistItem(CoreSailChecklistItemId.ADD_FAMILY, true, addFamilyDone),
            CoreSailChecklistItem(CoreSailChecklistItemId.PERMISSIONS, true, permissionsDone),
            CoreSailChecklistItem(CoreSailChecklistItemId.OFFLINE_TEST, true, offlineTestDone),
            CoreSailChecklistItem(CoreSailChecklistItemId.BACKUP, false, backupDone),
        )
        val done = items.count { it.done }
        return CoreSailChecklistReport(
            items = items,
            permissions = permissions,
            ready = ready,
            doneCount = done.toUInt(),
            totalCount = items.size.toUInt(),
            requiredDoneCount = items.count { it.required && it.done }.toUInt(),
            requiredTotalCount = items.count { it.required }.toUInt(),
        )
    }

    @Test
    fun `every step is listed with its own done or not-done mark`() {
        compose.setContent {
            CruiseMeshTheme {
                SailChecklistScreen(
                    report = report(
                        shorePassDone = true,
                        addFamilyDone = true,
                        permissions = listOf(
                            CoreSailPermissionRow(CoreSailPermission.BLUETOOTH, granted = true),
                            CoreSailPermissionRow(CoreSailPermission.NOTIFICATIONS, granted = false),
                            CoreSailPermissionRow(
                                CoreSailPermission.BATTERY_OPTIMIZATION,
                                granted = false,
                            ),
                        ),
                    ),
                    contactCount = 3,
                    onShorePass = {},
                    onAddFamily = {},
                    onGrantPermission = {},
                    onBackUp = {},
                    onBack = {},
                )
            }
        }

        // Scrolled to in turn: the list is longer than the test window, which
        // is the ordinary case on a phone too.
        for (step in listOf(
            "Set up your Shore Pass",
            "Add your family",
            "Let it run in your pocket",
            "Send a message with no internet",
            "Back up your identity",
            // The count the "add your family" row is asked to show.
            "3 people added",
        )) {
            compose.onNodeWithText(step).performScrollTo().assertIsDisplayed()
        }
        // Two steps done plus one grant given; five steps and grants still open.
        assertEquals(3, compose.onAllNodesWithContentDescription("Done").fetchSemanticsNodes().size)
        assertEquals(
            5,
            compose.onAllNodesWithContentDescription("Not done yet").fetchSemanticsNodes().size,
        )
        compose.onNodeWithText("2 of 5 done").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `each permission sub-row opens its own grant`() {
        var requested: CoreSailPermission? = null
        compose.setContent {
            CruiseMeshTheme {
                SailChecklistScreen(
                    report = report(),
                    contactCount = 0,
                    onShorePass = {},
                    onAddFamily = {},
                    onGrantPermission = { requested = it },
                    onBackUp = {},
                    onBack = {},
                )
            }
        }

        compose.onNodeWithText("Notifications").performScrollTo().performClick()
        assertEquals(CoreSailPermission.NOTIFICATIONS, requested)

        compose.onNodeWithText("Battery use").performScrollTo().performClick()
        assertEquals(CoreSailPermission.BATTERY_OPTIMIZATION, requested)
    }

    /** Optional steps left undone never contradict the core's "ready". */
    @Test
    fun `a ready report says so even with optional steps open`() {
        compose.setContent {
            CruiseMeshTheme {
                SailChecklistScreen(
                    report = report(
                        addFamilyDone = true,
                        permissionsDone = true,
                        offlineTestDone = true,
                        permissions = listOf(
                            CoreSailPermissionRow(CoreSailPermission.BLUETOOTH, granted = true),
                            CoreSailPermissionRow(CoreSailPermission.NOTIFICATIONS, granted = true),
                            CoreSailPermissionRow(
                                CoreSailPermission.BATTERY_OPTIMIZATION,
                                granted = true,
                            ),
                        ),
                        ready = true,
                    ),
                    contactCount = 2,
                    onShorePass = {},
                    onAddFamily = {},
                    onGrantPermission = {},
                    onBackUp = {},
                    onBack = {},
                )
            }
        }

        compose.onNodeWithText("You're set to sail.").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Back up your identity").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `the home card shows progress and can be waved away`() {
        var opened = 0
        var dismissed = 0
        compose.setContent {
            CruiseMeshTheme {
                SailChecklistCard(
                    progress = SailChecklistProgress(doneCount = 2, totalCount = 5),
                    onClick = { opened += 1 },
                    onDismiss = { dismissed += 1 },
                )
            }
        }

        compose.onNodeWithText("Before you sail: 2 of 5 done").assertIsDisplayed().performClick()
        assertEquals(1, opened)
        compose.onNodeWithText("Dismiss").performClick()
        assertEquals(1, dismissed)
    }

    @Test
    fun `the back arrow is reachable`() {
        var backs = 0
        compose.setContent {
            CruiseMeshTheme {
                SailChecklistScreen(
                    report = report(),
                    contactCount = 0,
                    onShorePass = {},
                    onAddFamily = {},
                    onGrantPermission = {},
                    onBackUp = {},
                    onBack = { backs += 1 },
                )
            }
        }

        compose.onNodeWithContentDescription("Back").performClick()
        assertEquals(1, backs)
    }
}
