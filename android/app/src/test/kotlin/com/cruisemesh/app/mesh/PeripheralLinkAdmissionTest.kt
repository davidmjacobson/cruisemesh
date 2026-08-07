package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The inbound half of the ACL budget. The properties that matter are the cap
 * boundary itself, that an established link is never displaced by a newer one,
 * and that a slot comes back only when a link genuinely goes away.
 */
class PeripheralLinkAdmissionTest {

    @Test
    fun `admits up to the cap and turns the next arrival away`() {
        val admission = PeripheralLinkAdmission(maxLinks = 3)

        assertEquals(PeripheralAdmissionDecision.Admitted(1), admission.admit("a"))
        assertEquals(PeripheralAdmissionDecision.Admitted(2), admission.admit("b"))
        assertEquals(PeripheralAdmissionDecision.Admitted(3), admission.admit("c"))

        val rejected = admission.admit("d")
        assertEquals(PeripheralAdmissionDecision.Rejected(3), rejected)
        assertFalse(admission.holds("d"))
        assertEquals(3, admission.activeCount())
    }

    @Test
    fun `a rejection never displaces an established link`() {
        // The dense-fleet churn rule from #148, in the inbound direction: at
        // the margin the newest arrival loses, so the link set converges
        // instead of every phone endlessly evicting every other phone's links.
        val admission = PeripheralLinkAdmission(maxLinks = 2)
        admission.admit("established-a")
        admission.admit("established-b")

        repeat(20) { attempt -> admission.admit("newcomer-$attempt") }

        assertTrue(admission.holds("established-a"))
        assertTrue(admission.holds("established-b"))
        assertEquals(2, admission.activeCount())
    }

    @Test
    fun `a duplicate connect callback for a held address does not consume a second slot`() {
        // The GATT stack can deliver a repeat STATE_CONNECTED for a device it
        // has already reported; treating that as a fresh admission would leak
        // a slot and, at the cap, reject a link we are already serving.
        val admission = PeripheralLinkAdmission(maxLinks = 2)
        assertEquals(PeripheralAdmissionDecision.Admitted(1), admission.admit("a"))
        assertEquals(PeripheralAdmissionDecision.AlreadyHeld(1), admission.admit("a"))
        assertEquals(1, admission.activeCount())

        // ...and still not at the cap, so a genuinely new central gets in.
        assertEquals(PeripheralAdmissionDecision.Admitted(2), admission.admit("b"))
    }

    @Test
    fun `a duplicate connect callback at the cap is admitted, not rejected`() {
        val admission = PeripheralLinkAdmission(maxLinks = 1)
        admission.admit("a")
        assertEquals(PeripheralAdmissionDecision.AlreadyHeld(1), admission.admit("a"))
        assertTrue(admission.holds("a"))
    }

    @Test
    fun `releasing a link frees its slot for whoever connects next`() {
        val admission = PeripheralLinkAdmission(maxLinks = 2)
        admission.admit("a")
        admission.admit("b")
        assertEquals(PeripheralAdmissionDecision.Rejected(2), admission.admit("c"))

        assertTrue(admission.release("a"))
        assertEquals(PeripheralAdmissionDecision.Admitted(2), admission.admit("c"))
    }

    @Test
    fun `releasing an address that holds nothing is a no-op`() {
        // tearDownLink is idempotent per address and also runs for centrals
        // that were turned away and so never held a slot; neither may hand a
        // slot back that was never taken.
        val admission = PeripheralLinkAdmission(maxLinks = 2)
        admission.admit("a")

        assertFalse(admission.release("never-admitted"))
        assertTrue(admission.release("a"))
        assertFalse(admission.release("a"))
        assertEquals(0, admission.activeCount())
    }

    @Test
    fun `a rejected address can be admitted later without any explicit reset`() {
        // A central turned away at the cap reconnects on its next scan hit;
        // once a slot frees, that reconnect must be an ordinary admission.
        val admission = PeripheralLinkAdmission(maxLinks = 1)
        admission.admit("held")
        assertEquals(PeripheralAdmissionDecision.Rejected(1), admission.admit("turned-away"))

        admission.release("held")
        assertEquals(PeripheralAdmissionDecision.Admitted(1), admission.admit("turned-away"))
    }

    @Test
    fun `clearAll releases every slot for a restarted peripheral role`() {
        val admission = PeripheralLinkAdmission(maxLinks = 2)
        admission.admit("a")
        admission.admit("b")

        admission.clearAll()

        assertEquals(0, admission.activeCount())
        assertEquals(PeripheralAdmissionDecision.Admitted(1), admission.admit("c"))
    }

    @Test(expected = IllegalArgumentException::class)
    fun `a zero cap is rejected rather than silently refusing every link`() {
        PeripheralLinkAdmission(maxLinks = 0)
    }
}
