package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * How the peripheral spray cooldown composes with the per-peer failover
 * debounce from #269 -- the two brakes sit on the same burst and must not
 * duplicate or cancel each other.
 *
 * They answer different questions and are keyed differently on purpose:
 * [PeripheralSprayCooldown] is keyed by *link address* and asks "may this
 * link's burst go out yet"; [FailoverResumeDebounce] is keyed by *logical
 * peer* and asks "have this peer's links finished dying". `MeshService`
 * re-enters the deferred burst through the debounce rather than running it
 * directly, which is what makes the two compose instead of stack, so these
 * tests drive them in exactly that order.
 */
class PeripheralSprayCooldownDebounceTest {

    private val peer = "aabbccddeeff0011"

    @Test
    fun `the cooldown outlasts the debounce window it re-enters through`() {
        // If the cooldown were the shorter of the two, the deferred burst would
        // land inside the same disconnect burst the debounce is still
        // coalescing, and the brake would be doing nothing the debounce was not
        // already doing.
        assertTrue(PeripheralSprayCooldown.DEFAULT_WINDOW_MS > FailoverResumeDebounce().windowMs)
    }

    @Test
    fun `a deferred burst re-enters through the debounce and runs once`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        val debounce = FailoverResumeDebounce(windowMs = 300)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)

        // The peer reconnects 200ms later and HELLOs; the burst is held for
        // what is left of the window, not for a fresh one.
        val deferralMs = cooldown.deferralMs("aa:bb", nowMs = 1_200)
        assertEquals(4_800L, deferralMs)

        // The deferral timer fires and hands the peer to the debounce, which
        // schedules exactly one resume.
        val firesAtMs = 1_200 + deferralMs
        val arm = requireNotNull(debounce.request(peer, nowMs = firesAtMs))
        assertEquals(300L, arm.delayMs)
        debounce.fired(peer, arm.token)

        // ...and by then the window has lapsed, so the resume's own burst is
        // not held back a second time.
        assertEquals(0L, cooldown.deferralMs("aa:bb", nowMs = firesAtMs + arm.delayMs))
    }

    @Test
    fun `a deferral landing on a live failover window is absorbed into it`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        val debounce = FailoverResumeDebounce(windowMs = 300)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)
        val deferralFiresAtMs = 1_000 + cooldown.deferralMs("aa:bb", nowMs = 1_000)

        // The link dies again while the cooldown is running -- an ordinary
        // failover, which arms the debounce for this peer.
        val failover = requireNotNull(debounce.request(peer, nowMs = deferralFiresAtMs - 100))

        // The deferral timer now fires into that live window. It must not
        // schedule a second resume: the armed one already covers this peer, and
        // two overlapping multi-KB bursts on one link is the failure #269 fixed.
        assertNull(debounce.request(peer, nowMs = deferralFiresAtMs))
        assertTrue(debounce.isPending(peer))

        debounce.fired(peer, failover.token)
    }

    @Test
    fun `both links of one peer under cooldown still resume that peer once`() {
        // A phone reachable over both BLE halves can have two inbound
        // addresses. The cooldown is per address, so each defers on its own
        // schedule -- the debounce is what collapses them back to one burst for
        // the one logical peer.
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        val debounce = FailoverResumeDebounce(windowMs = 300)
        cooldown.armAfterRejectTeardown("link-1", nowMs = 1_000)
        cooldown.armAfterRejectTeardown("link-2", nowMs = 1_150)

        assertNotNull(debounce.request(peer, nowMs = 6_000))
        assertNull(debounce.request(peer, nowMs = 6_150))
    }

    @Test
    fun `a window re-armed mid-deferral holds the re-entry back again`() {
        // The deferral does not fire the burst itself -- it re-enters the one
        // resume path, which re-reads the window rather than treating its own
        // timer as proof the link settled. If the link failed a second time
        // while the deferral was counting down, the burst waits out the new
        // window instead of landing on a link that had just broken again.
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        cooldown.armAfterRejectTeardown("aa:bb", nowMs = 1_000)
        val firesAtMs = 1_000 + cooldown.deferralMs("aa:bb", nowMs = 1_000)

        cooldown.armAfterRejectTeardown("aa:bb", nowMs = firesAtMs - 500)

        assertEquals(4_500L, cooldown.deferralMs("aa:bb", nowMs = firesAtMs))
    }

    @Test
    fun `one address's cooldown never holds back another peer's resume`() {
        val cooldown = PeripheralSprayCooldown(windowMs = 5_000)
        val debounce = FailoverResumeDebounce(windowMs = 300)
        cooldown.armAfterRejectTeardown("noisy", nowMs = 1_000)

        assertEquals(0L, cooldown.deferralMs("quiet", nowMs = 1_000))
        assertNotNull(debounce.request("00112233445566ff", nowMs = 1_000))
    }
}
