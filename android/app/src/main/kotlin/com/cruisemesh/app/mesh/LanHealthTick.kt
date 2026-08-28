package com.cruisemesh.app.mesh

/**
 * What one periodic LAN pass has to act on, pulled out of `MeshService` so it
 * can be pinned by tests.
 *
 * The defect these exist for was never a wrong predicate — it was a missing
 * call site. §10 step 5's roster notice had exactly one carrier (an inbound
 * HELLO2), and a link to a device of this person's own was in none of the
 * accessors the health pass read, so nothing probed it and nothing re-offered
 * on it. A test that only exercises the schedule class still passes when the
 * pass forgets to consult it, which is precisely how the hole got in. These
 * functions are the selection step of the pass itself.
 */

/**
 * The LAN addresses one health tick must probe.
 *
 * Every identified LAN route, **plus every live link to a device of this
 * person's own** — which is never a route, so it was in none of the accessors
 * the loop used to read. A link nothing probes is a link nothing closes:
 * established links run with no socket read timeout, so a half-open one held
 * its socket, carried no frames, and (being a live connection) told the LAN
 * transport it had company, for the whole Wi-Fi join. That is the state an
 * approving phone sat in for 26 minutes while the device it had removed waited
 * to be told.
 */
internal fun lanHealthProbeAddresses(
    identifiedRoutes: List<MeshRouterState.IdentifiedRoute>,
    ownDeviceLinks: List<Pair<MeshRouterState.Transport, String>>,
): List<String> {
    val addresses = identifiedRoutes
        .asSequence()
        .filter { it.transport == MeshRouterState.Transport.LAN }
        .map { it.address }
        .toMutableList()
    ownDeviceLinks
        .asSequence()
        .filter { (transport, _) -> transport == MeshRouterState.Transport.LAN }
        .mapTo(addresses) { (_, address) -> address }
    return addresses
}

/**
 * The own-device links this tick owes §10 step 5's roster, each with the
 * capability bits to send it under. See [OwnRosterNoticeSchedule].
 */
internal fun ownRosterNoticeTargets(
    ownDeviceLinks: List<Pair<MeshRouterState.Transport, String>>,
    schedule: OwnRosterNoticeSchedule,
    nowMs: Long,
): List<Pair<String, UInt>> = ownDeviceLinks
    .asSequence()
    .filter { (transport, _) -> transport == MeshRouterState.Transport.LAN }
    .mapNotNull { (_, address) ->
        schedule.dueCapabilities(address, nowMs)?.let { address to it }
    }
    .toList()

/**
 * Own-device LAN links whose peer HELLO2 has not arrived, and which are
 * therefore owed another of ours.
 *
 * The re-offer above is level-triggered, but its precondition — what the peer
 * says it can read — still crosses the wire exactly once, on a single frame,
 * at link establishment. A HELLO2 lost to a reordering or a link that came up
 * mid-write leaves the link permanently ineligible for the notice, which is the
 * same single-delivered-event failure the level-trigger was added to remove. So
 * the tick nudges: our HELLO2 again, bounded ([OwnRosterNoticeSchedule.NUDGE_LIMIT]),
 * stopping the moment theirs arrives.
 */
internal fun ownDeviceLinksAwaitingHello2(
    ownDeviceLinks: List<Pair<MeshRouterState.Transport, String>>,
    schedule: OwnRosterNoticeSchedule,
): List<String> = ownDeviceLinks
    .asSequence()
    .filter { (transport, _) -> transport == MeshRouterState.Transport.LAN }
    .filter { (_, address) -> schedule.claimHello2Nudge(address) }
    .map { (_, address) -> address }
    .toList()
