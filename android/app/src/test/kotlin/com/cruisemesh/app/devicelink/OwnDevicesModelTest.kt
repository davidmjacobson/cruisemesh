package com.cruisemesh.app.devicelink

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The "Your devices" mapping, pinned against the two refusals core actually
 * makes (`core_revoke_devices_roster`: the approving device may not bury itself,
 * and a person must keep at least one device).
 *
 * These are the tests that stop the screen offering a tap that would fail. The
 * failure they guard against is not a crash — it is a person reading a
 * confirmation dialog about losing a phone, tapping through it, and being told
 * afterwards that nothing happened.
 */
class OwnDevicesModelTest {

    private fun id(byte: Int) = ByteArray(16) { byte.toByte() }

    private val phone = id(1)
    private val tablet = id(2)
    private val laptop = id(3)

    @Test
    fun `a lone device is this device, approves, and cannot be removed`() {
        val rows = ownDeviceRows(listOf(phone), approvingDeviceId = phone, ownDeviceId = phone)

        assertEquals(1, rows.size)
        assertTrue(rows[0].isThisDevice)
        assertTrue(rows[0].approves)
        assertFalse(rows[0].removable)
        assertEquals(RemoveDeviceBlock.LAST_DEVICE, removeBlockedReason(rows, rows[0]))
    }

    @Test
    fun `the approving device may remove a sibling but never itself`() {
        val rows = ownDeviceRows(
            listOf(phone, tablet),
            approvingDeviceId = phone,
            ownDeviceId = phone,
        )

        val self = rows.single { it.isThisDevice }
        val sibling = rows.single { !it.isThisDevice }
        assertFalse(self.removable)
        assertEquals(RemoveDeviceBlock.IS_THE_APPROVING_DEVICE, removeBlockedReason(rows, self))
        assertTrue(sibling.removable)
        assertNull(removeBlockedReason(rows, sibling))
    }

    @Test
    fun `a device that does not approve is offered no removals at all`() {
        // Read from the tablet: §10.1's update can only be signed by the phone.
        val rows = ownDeviceRows(
            listOf(phone, tablet, laptop),
            approvingDeviceId = phone,
            ownDeviceId = tablet,
        )

        assertTrue(rows.none { it.removable })
        for (row in rows.filterNot { it.approves }) {
            assertEquals(
                RemoveDeviceBlock.NOT_THE_APPROVING_DEVICE,
                removeBlockedReason(rows, row),
            )
        }
    }

    @Test
    fun `positions follow roster order so their numbering is stable`() {
        val rows = ownDeviceRows(
            listOf(phone, tablet, laptop),
            approvingDeviceId = phone,
            ownDeviceId = laptop,
        )

        assertEquals(listOf(1, 2, 3), rows.map { it.position })
        assertEquals(3, rows.single { it.isThisDevice }.position)
    }

    @Test
    fun `an install with no device keys of its own is simply not in the list`() {
        val rows = ownDeviceRows(listOf(phone, tablet), approvingDeviceId = phone, ownDeviceId = null)

        assertTrue(rows.none { it.isThisDevice })
        // And it may not remove anything: it cannot be the approving device.
        assertTrue(rows.none { it.removable })
    }

    @Test
    fun `shape distinguishes never-linked from a one-device roster`() {
        val alone = ownDeviceRows(listOf(phone), phone, phone)
        assertEquals(
            YourDevicesShape.NEVER_LINKED,
            yourDevicesShape(hasRoster = false, rows = emptyList()),
        )
        assertEquals(YourDevicesShape.ONLY_THIS_DEVICE, yourDevicesShape(true, alone))
        assertEquals(
            YourDevicesShape.SEVERAL,
            yourDevicesShape(true, ownDeviceRows(listOf(phone, tablet), phone, phone)),
        )
    }

    @Test
    fun `Add a device is offered only where the roster can be signed`() {
        // Nothing linked yet: this install is about to become device one.
        assertTrue(canAddDevice(hasRoster = false, rows = emptyList()))

        val fromPhone = ownDeviceRows(listOf(phone, tablet), phone, phone)
        assertTrue(canAddDevice(hasRoster = true, rows = fromPhone))

        // From the tablet the ceremony would fail at §9.5's signature, at the
        // very end, after two people had compared six digits.
        val fromTablet = ownDeviceRows(listOf(phone, tablet), phone, tablet)
        assertFalse(canAddDevice(hasRoster = true, rows = fromTablet))

        // An install with a roster it is not in cannot sign it either.
        val stranger = ownDeviceRows(listOf(phone, tablet), phone, null)
        assertFalse(canAddDevice(hasRoster = true, rows = stranger))
    }

    @Test
    fun `a never-linked install still has a row for this phone`() {
        val row = thisDeviceOnlyRow(phone)

        assertTrue(row.isThisDevice)
        assertFalse(row.approves)
        assertFalse(row.removable)
        assertEquals(1, row.position)
        // And with no device key at all there is simply no code to show.
        assertEquals("", thisDeviceOnlyRow(null).deviceIdHex)
    }

    @Test
    fun `the short code is the tail of the id, spaced, and never the whole thing`() {
        val rows = ownDeviceRows(listOf(phone), phone, phone)
        val code = shortDeviceCode(rows[0].deviceIdHex)

        assertEquals("0101 0101", code)
        assertEquals(32, rows[0].deviceIdHex.length)
    }
}
