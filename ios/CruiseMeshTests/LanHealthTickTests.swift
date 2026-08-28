import XCTest
@testable import CruiseMesh

/// The periodic LAN pass's selection step.
///
/// These exist because the defect was never a wrong predicate: it was a missing
/// call site. A link to a device of this person's own is in none of the route
/// accessors, so nothing probed it and nothing re-offered §10 step 5's roster on
/// it — while a test of the schedule class in isolation went on passing. What
/// has to be held in place is that the pass *consults* those links at all.
///
/// Mirrors Android's `LanHealthTickTest`.
final class LanHealthTickTests: XCTestCase {
    private let capable = OwnRosterNoticePolicy.capabilityBit

    private func route(
        _ address: String,
        _ transport: MeshRouterState.Transport
    ) -> MeshRouterState.IdentifiedRoute {
        MeshRouterState.IdentifiedRoute(transport: transport, address: address, userId: Data([1]))
    }

    func testTheHealthPassProbesOwnDeviceLinksNotOnlyRoutes() {
        let addresses = lanHealthProbeAddresses(
            identifiedRoutes: [route("friend-lan", .lan), route("friend-ble", .central)],
            ownDeviceLinks: [
                (transport: .lan, address: "our-other-phone"),
                (transport: .peripheral, address: "our-other-phone-ble")
            ]
        )

        XCTAssertEqual(addresses, ["friend-lan", "our-other-phone"])
    }

    func testAnOwnDeviceLinkIsReofferedTheRosterOnTheIntervalNotOnce() {
        let schedule = OwnRosterNoticeSchedule()
        let links: [(transport: MeshRouterState.Transport, address: String)] = [
            (transport: .lan, address: "our-other-phone")
        ]
        schedule.noteHello2(address: "our-other-phone", capabilities: capable)

        // The meeting: due at once.
        let first = ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_000)
        XCTAssertEqual(first.map { $0.address }, ["our-other-phone"])
        XCTAssertEqual(first.map { $0.capabilities }, [capable])
        schedule.noteOffered(address: "our-other-phone", nowMs: 1_000)

        // A removal seconds later, on the link that is already up: nothing
        // re-HELLOs, so this is the only thing that can carry it.
        XCTAssertTrue(
            ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_500).isEmpty
        )
        XCTAssertEqual(
            ownRosterNoticeTargets(
                ownDeviceLinks: links,
                schedule: schedule,
                nowMs: 1_000 + coreOwnRosterNoticeReofferIntervalMs()
            ).count,
            1
        )
    }

    func testAPeerThatCannotReadANoticeIsNeverOfferedOne() {
        let schedule = OwnRosterNoticeSchedule()
        let links: [(transport: MeshRouterState.Transport, address: String)] = [
            (transport: .lan, address: "our-other-phone")
        ]
        schedule.noteHello2(address: "our-other-phone", capabilities: 0)

        XCTAssertTrue(
            ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_000).isEmpty
        )
    }

    func testALinkWhosePeerHello2NeverArrivedIsNudgedButNotForever() {
        let schedule = OwnRosterNoticeSchedule()
        let links: [(transport: MeshRouterState.Transport, address: String)] = [
            (transport: .lan, address: "our-other-phone")
        ]

        // No capability record: this link can never become eligible for a
        // notice, which is the same one-delivered-event failure the
        // level-trigger exists to remove.
        XCTAssertTrue(
            ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_000).isEmpty
        )
        for _ in 0..<OwnRosterNoticeSchedule.nudgeLimit {
            XCTAssertEqual(
                ownDeviceLinksAwaitingHello2(ownDeviceLinks: links, schedule: schedule),
                ["our-other-phone"]
            )
        }
        XCTAssertTrue(
            ownDeviceLinksAwaitingHello2(ownDeviceLinks: links, schedule: schedule).isEmpty
        )

        // And the moment their HELLO2 does land, the nudging stops and the
        // notice becomes due.
        schedule.noteHello2(address: "our-other-phone", capabilities: capable)
        XCTAssertTrue(
            ownDeviceLinksAwaitingHello2(ownDeviceLinks: links, schedule: schedule).isEmpty
        )
        XCTAssertEqual(
            ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_000).count,
            1
        )
    }

    func testASendThatNeverLeftThePhoneDoesNotRestartTheTimer() {
        let schedule = OwnRosterNoticeSchedule()
        let links: [(transport: MeshRouterState.Transport, address: String)] = [
            (transport: .lan, address: "our-other-phone")
        ]
        schedule.noteHello2(address: "our-other-phone", capabilities: capable)

        // The caller only books an offer the router accepted, so a half-open
        // link is retried on the next tick instead of waiting out an interval
        // it was never told anything in.
        XCTAssertEqual(
            ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_000).count,
            1
        )
        XCTAssertEqual(
            ownRosterNoticeTargets(ownDeviceLinks: links, schedule: schedule, nowMs: 1_001).count,
            1
        )
    }
}
