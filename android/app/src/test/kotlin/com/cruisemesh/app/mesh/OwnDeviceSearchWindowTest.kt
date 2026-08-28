package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.coreLanOwnDeviceSearchWindowMs

/**
 * The sweep motive that goes looking for one of this person's own devices has
 * to stop, exactly as the contact-side motive decays.
 *
 * `specs/multi-device-v1.md` §10 step 5. A second phone that is switched off or
 * left at home is missing forever; unbounded, this would sweep the /24 every
 * five minutes on every Wi-Fi the person joins, on battery, for the whole life
 * of each join — and it would do so for precisely the multi-device households
 * the mechanism was added for.
 */
class OwnDeviceSearchWindowTest {

    private val windowMs = coreLanOwnDeviceSearchWindowMs()

    private fun roster(vararg ids: Int): String =
        ownRosterFingerprint(ids.map { byteArrayOf(it.toByte()) })

    @Test
    fun `a sibling that goes missing is searched for, and the search runs out`() {
        val window = OwnDeviceSearchWindow()
        val fleet = roster(1, 2)

        // Both devices linked: nothing to look for.
        window.observe(fleet, unlinkedOwnDevices = 0, nowMs = 0)
        // (The very first observation is a roster change, so it arms; the
        // second, unchanged and with nothing missing, does not.)
        window.observe(fleet, unlinkedOwnDevices = 0, nowMs = windowMs)
        assertFalse(window.isLive())

        // The sibling link drops.
        window.observe(fleet, unlinkedOwnDevices = 1, nowMs = windowMs)
        assertTrue(window.isLive())

        // Still missing an hour later -- and no longer searched for.
        window.observe(fleet, unlinkedOwnDevices = 1, nowMs = windowMs + windowMs - 1)
        assertTrue(window.isLive())
        window.observe(fleet, unlinkedOwnDevices = 1, nowMs = windowMs * 4)
        assertFalse("a phone left at home kept the subnet sweep running", window.isLive())
    }

    @Test
    fun `a removal sends the approving phone looking for the device it removed`() {
        val window = OwnDeviceSearchWindow()
        // Two devices, both linked, nothing to look for.
        window.observe(roster(1, 2), unlinkedOwnDevices = 0, nowMs = 0)
        window.observe(roster(1, 2), unlinkedOwnDevices = 0, nowMs = windowMs)
        assertFalse(window.isLive())

        // "Remove device": the removed one leaves this phone's roster at once,
        // so the shortfall does not rise -- it stays zero. Only the roster
        // change itself can send this phone looking for the device it must
        // still hand §10 step 5's notice to.
        window.observe(roster(1), unlinkedOwnDevices = 0, nowMs = windowMs)
        assertTrue("the approver had no motive to find the phone it silenced", window.isLive())

        // Bounded like every other reason to search.
        window.observe(roster(1), unlinkedOwnDevices = 0, nowMs = windowMs * 2)
        assertFalse(window.isLive())
    }

    @Test
    fun `joining a Wi-Fi network is a fresh reason to look`() {
        val window = OwnDeviceSearchWindow()
        window.observe(roster(1, 2), unlinkedOwnDevices = 1, nowMs = 0)
        window.observe(roster(1, 2), unlinkedOwnDevices = 1, nowMs = windowMs * 3)
        assertFalse(window.isLive())

        window.rearm(windowMs * 3)
        assertTrue(window.isLive())
    }

    @Test
    fun `a three-device person does not sweep for ever`() {
        // The transport keeps one own-device link at a time, so with three
        // devices the shortfall can never reach zero. A bare "someone is
        // missing" motive would therefore be permanent, not merely long.
        val window = OwnDeviceSearchWindow()
        val fleet = roster(1, 2, 3)
        window.observe(fleet, unlinkedOwnDevices = 2, nowMs = 0)
        assertTrue(window.isLive())
        window.observe(fleet, unlinkedOwnDevices = 1, nowMs = windowMs - 1)
        assertTrue(window.isLive())
        window.observe(fleet, unlinkedOwnDevices = 1, nowMs = windowMs)
        assertFalse(window.isLive())
    }

    @Test
    fun `the roster fingerprint ignores order and notices a removal`() {
        assertEquals(roster(2, 1), roster(1, 2))
        assertTrue(roster(1, 2) != roster(1))
    }

    @Test
    fun `clearing forgets the search`() {
        val window = OwnDeviceSearchWindow()
        window.observe(roster(1, 2), unlinkedOwnDevices = 1, nowMs = 0)
        assertTrue(window.isLive())
        window.clear()
        assertFalse(window.isLive())
    }
}
