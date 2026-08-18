package com.cruisemesh.app.devicelink

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.ui.CruiseMeshTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Every state "Your devices" can be in, drawn from rows rather than a store, so
 * the states core would have to be coaxed into producing are still reachable
 * from a test.
 */
@RunWith(AndroidJUnit4::class)
class YourDevicesUiTest {
    @get:Rule
    val compose = createComposeRule()

    private fun id(byte: Int) = ByteArray(16) { byte.toByte() }

    private fun items(
        deviceIds: List<ByteArray>,
        approving: ByteArray,
        own: ByteArray?,
        names: Map<Int, String> = emptyMap(),
    ): List<YourDeviceListItem> =
        ownDeviceRows(deviceIds, approving, own).map { row ->
            YourDeviceListItem(row = row, name = names[row.position].orEmpty(), firstSeenMs = null)
        }

    @Test
    fun `an install that has never linked shows this phone and offers Add a device`() {
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.NEVER_LINKED,
                    items = listOf(
                        YourDeviceListItem(thisDeviceOnlyRow(id(1)), name = "", firstSeenMs = null),
                    ),
                    canAddDevice = true,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = {},
                )
            }
        }

        compose.onNodeWithText("This is the only device signed in as you", substring = true)
            .assertIsDisplayed()
        // The screen says "this is the only device" and now shows it, rather
        // than leaving an empty space under its own sentence.
        compose.onNodeWithText("This phone").assertIsDisplayed()
        assertNothingDescribed("Remove This phone")
        compose.onNodeWithText("Add a device").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `a phone that cannot sign the roster is not offered Add a device`() {
        val phone = id(1)
        val tablet = id(2)
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.SEVERAL,
                    // Read from the tablet, which cannot sign §9.5's roster.
                    items = items(
                        listOf(phone, tablet),
                        approving = phone,
                        own = tablet,
                        names = mapOf(1 to "Kitchen phone"),
                    ),
                    canAddDevice = false,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = {},
                )
            }
        }

        assertEquals(
            0,
            compose.onAllNodesWithText("Add a device").fetchSemanticsNodes().size,
        )
        compose.onNodeWithText("Only Kitchen phone can add a device", substring = true)
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `a row with no Remove says why, and names the phone that can`() {
        val phone = id(1)
        val tablet = id(2)
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.SEVERAL,
                    items = items(
                        listOf(phone, tablet),
                        approving = phone,
                        own = tablet,
                        names = mapOf(1 to "Kitchen phone"),
                    ),
                    canAddDevice = false,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = {},
                )
            }
        }

        // Which device to use, and what to do when that device is the one that
        // is gone -- which is why a person came looking for Remove at all.
        compose.onNodeWithText("Only Kitchen phone can remove a device", substring = true)
            .assertIsDisplayed()
        // The recovery path is named on the row and again where Add a device
        // would have been -- both of them are dead ends without that phone.
        assertEquals(
            2,
            compose.onAllNodesWithText("contact support", substring = true)
                .fetchSemanticsNodes()
                .size,
        )
    }

    @Test
    fun `this phone is named as such and the approver carries the badge`() {
        val phone = id(1)
        val tablet = id(2)
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.SEVERAL,
                    items = items(listOf(phone, tablet), approving = phone, own = phone),
                    canAddDevice = true,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = {},
                )
            }
        }

        compose.onNodeWithText("This phone").assertIsDisplayed()
        compose.onNodeWithText("Device 2").assertIsDisplayed()
        compose.onNodeWithText("Approves new devices").assertIsDisplayed()
    }

    @Test
    fun `a name the person typed replaces the default one`() {
        val phone = id(1)
        val tablet = id(2)
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.SEVERAL,
                    items = items(
                        listOf(phone, tablet),
                        approving = phone,
                        own = phone,
                        names = mapOf(2 to "Kitchen tablet"),
                    ),
                    canAddDevice = true,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = {},
                )
            }
        }

        compose.onNodeWithText("Kitchen tablet").assertIsDisplayed()
        compose.onNodeWithContentDescription("Remove Kitchen tablet").assertIsDisplayed()
    }

    @Test
    fun `Remove is offered for a sibling and withheld from the approving device`() {
        val phone = id(1)
        val tablet = id(2)
        var removed: OwnDeviceRow? = null
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.SEVERAL,
                    items = items(listOf(phone, tablet), approving = phone, own = phone),
                    canAddDevice = true,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = { removed = it.row },
                )
            }
        }

        // The approving device has no Remove at all -- core would refuse it.
        assertNothingDescribed("Remove This phone")
        compose.onNodeWithContentDescription("Remove Device 2").performClick()

        assertEquals(2, removed?.position)
    }

    @Test
    fun `a phone that does not approve is offered no removals`() {
        val phone = id(1)
        val tablet = id(2)
        compose.setContent {
            CruiseMeshTheme {
                YourDevicesContent(
                    shape = YourDevicesShape.SEVERAL,
                    // Read from the tablet: §10.1's update needs the phone's key.
                    items = items(listOf(phone, tablet), approving = phone, own = tablet),
                    canAddDevice = false,
                    onAddDevice = {},
                    onRename = {},
                    onRemove = {},
                )
            }
        }

        assertNothingDescribed("Remove Device 1")
        assertNothingDescribed("Remove This phone")
    }

    /** Asserts nothing on screen carries this description. */
    private fun assertNothingDescribed(label: String) {
        assertEquals(
            0,
            compose.onAllNodes(hasContentDescription(label)).fetchSemanticsNodes().size,
        )
    }
}
