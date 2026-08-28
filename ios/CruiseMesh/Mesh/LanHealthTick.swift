import Foundation

/// What one periodic LAN pass has to act on, pulled out of `MeshController` so
/// it can be pinned by tests.
///
/// The defect these exist for was never a wrong predicate — it was a missing
/// call site. §10 step 5's roster notice had exactly one carrier (an inbound
/// HELLO2), and a link to a device of this person's own was in none of the
/// accessors the health pass read, so nothing probed it and nothing re-offered
/// on it. A test that only exercises the schedule class still passes when the
/// pass forgets to consult it, which is precisely how the hole got in. These
/// functions are the selection step of the pass itself.
///
/// Mirrors Android's `LanHealthTick.kt`.

/// The LAN addresses one health tick must probe.
///
/// Every identified LAN route, **plus every live link to a device of this
/// person's own** — which is never a route, so it was in none of the accessors
/// the loop used to read. A link nothing probes is a link nothing closes: a
/// half-open one held its connection, carried no frames, and (being live) told
/// the LAN transport it had company, for the whole Wi-Fi join. That is the
/// state an approving phone sat in for 26 minutes while the device it had
/// removed waited to be told.
func lanHealthProbeAddresses(
    identifiedRoutes: [MeshRouterState.IdentifiedRoute],
    ownDeviceLinks: [(transport: MeshRouterState.Transport, address: String)]
) -> [String] {
    identifiedRoutes.filter { $0.transport == .lan }.map(\.address)
        + ownDeviceLinks.filter { $0.transport == .lan }.map { $0.address }
}

/// The own-device links this tick owes §10 step 5's roster, each with the
/// capability bits to send it under. See `OwnRosterNoticeSchedule`.
func ownRosterNoticeTargets(
    ownDeviceLinks: [(transport: MeshRouterState.Transport, address: String)],
    schedule: OwnRosterNoticeSchedule,
    nowMs: Int64
) -> [(address: String, capabilities: UInt32)] {
    ownDeviceLinks.filter { $0.transport == .lan }.compactMap { link in
        guard let capabilities = schedule.dueCapabilities(address: link.address, nowMs: nowMs)
        else { return nil }
        return (address: link.address, capabilities: capabilities)
    }
}

/// Own-device LAN links whose peer HELLO2 has not arrived, and which are
/// therefore owed another of ours. See `OwnRosterNoticeSchedule.claimHello2Nudge`.
func ownDeviceLinksAwaitingHello2(
    ownDeviceLinks: [(transport: MeshRouterState.Transport, address: String)],
    schedule: OwnRosterNoticeSchedule
) -> [String] {
    ownDeviceLinks
        .filter { $0.transport == .lan }
        .filter { schedule.claimHello2Nudge(address: $0.address) }
        .map { $0.address }
}
