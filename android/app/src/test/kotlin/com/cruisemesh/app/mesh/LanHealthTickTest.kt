package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The periodic LAN pass's selection step.
 *
 * These exist because the defect was never a wrong predicate: it was a missing
 * call site. A link to a device of this person's own is in none of the route
 * accessors, so nothing probed it and nothing re-offered §10 step 5's roster on
 * it — while a test of the schedule class in isolation went on passing. What
 * has to be held in place is that the pass *consults* those links at all.
 */
class LanHealthTickTest {

    private fun route(address: String, transport: MeshRouterState.Transport) =
        MeshRouterState.IdentifiedRoute(transport, address, byteArrayOf(1))

    @Test
    fun `the health pass probes own-device links, not only routes`() {
        val addresses = lanHealthProbeAddresses(
            identifiedRoutes = listOf(
                route("friend-lan", MeshRouterState.Transport.LAN),
                route("friend-ble", MeshRouterState.Transport.CENTRAL),
            ),
            ownDeviceLinks = listOf(
                MeshRouterState.Transport.LAN to "our-other-phone",
                MeshRouterState.Transport.PERIPHERAL to "our-other-phone-ble",
            ),
        )

        assertEquals(listOf("friend-lan", "our-other-phone"), addresses)
    }

    @Test
    fun `an own-device link is re-offered the roster on the interval, not once`() {
        val schedule = OwnRosterNoticeSchedule()
        val links = listOf(MeshRouterState.Transport.LAN to "our-other-phone")
        schedule.noteHello2("our-other-phone", OwnRosterNoticePolicy.CAPABILITY_BIT)

        // The meeting: due at once.
        val first = ownRosterNoticeTargets(links, schedule, nowMs = 1_000)
        assertEquals(listOf("our-other-phone" to OwnRosterNoticePolicy.CAPABILITY_BIT), first)
        schedule.noteOffered("our-other-phone", 1_000)

        // A removal seconds later, on the link that is already up: nothing
        // re-HELLOs, so this is the only thing that can carry it.
        assertTrue(ownRosterNoticeTargets(links, schedule, nowMs = 1_500).isEmpty())
        assertEquals(1, ownRosterNoticeTargets(links, schedule, nowMs = 62_000).size)
    }

    @Test
    fun `a peer that cannot read a notice is never offered one`() {
        val schedule = OwnRosterNoticeSchedule()
        val links = listOf(MeshRouterState.Transport.LAN to "our-other-phone")
        schedule.noteHello2("our-other-phone", 0u)

        assertTrue(ownRosterNoticeTargets(links, schedule, nowMs = 1_000).isEmpty())
    }

    @Test
    fun `a link whose peer HELLO2 never arrived is nudged, but not for ever`() {
        val schedule = OwnRosterNoticeSchedule()
        val links = listOf(MeshRouterState.Transport.LAN to "our-other-phone")

        // No capability record: this link can never become eligible for a
        // notice, which is the same one-delivered-event failure the
        // level-trigger exists to remove.
        assertTrue(ownRosterNoticeTargets(links, schedule, nowMs = 1_000).isEmpty())
        repeat(OwnRosterNoticeSchedule.NUDGE_LIMIT) {
            assertEquals(listOf("our-other-phone"), ownDeviceLinksAwaitingHello2(links, schedule))
        }
        assertTrue(ownDeviceLinksAwaitingHello2(links, schedule).isEmpty())

        // And the moment their HELLO2 does land, the nudging stops and the
        // notice becomes due.
        schedule.noteHello2("our-other-phone", OwnRosterNoticePolicy.CAPABILITY_BIT)
        assertTrue(ownDeviceLinksAwaitingHello2(links, schedule).isEmpty())
        assertEquals(1, ownRosterNoticeTargets(links, schedule, nowMs = 1_000).size)
    }

    @Test
    fun `a send that never left the phone does not restart the timer`() {
        val schedule = OwnRosterNoticeSchedule()
        val links = listOf(MeshRouterState.Transport.LAN to "our-other-phone")
        schedule.noteHello2("our-other-phone", OwnRosterNoticePolicy.CAPABILITY_BIT)

        // The caller only books an offer the router accepted, so a half-open
        // link is retried on the next tick instead of waiting out an interval
        // it was never told anything in.
        assertEquals(1, ownRosterNoticeTargets(links, schedule, nowMs = 1_000).size)
        assertEquals(1, ownRosterNoticeTargets(links, schedule, nowMs = 1_001).size)
    }
}
