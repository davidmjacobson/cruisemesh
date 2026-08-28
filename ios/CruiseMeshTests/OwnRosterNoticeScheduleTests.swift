import XCTest
@testable import CruiseMesh

/// §10 step 5's re-offer schedule -- the half of the removal notice that was
/// missing, and the reason a phone that had been removed from its person's
/// devices went on believing it was linked while sitting on the same Wi-Fi as
/// the phone that removed it.
///
/// The Swift twin of Android's `OwnRosterNoticeScheduleTest`.
final class OwnRosterNoticeScheduleTests: XCTestCase {
    private let capable = OwnRosterNoticePolicy.capabilityBit
    private let reofferIntervalMs = coreOwnRosterNoticeReofferIntervalMs()

    func testALinkThatHasJustSaidHelloIsOwedTheRosterImmediately() {
        let schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2(address: "lan:a", capabilities: capable)
        XCTAssertEqual(schedule.dueCapabilities(address: "lan:a", nowMs: 1_000), capable)
    }

    func testALinkNobodyHasSaidHelloOnIsOwedNothing() {
        let schedule = OwnRosterNoticeSchedule()
        XCTAssertNil(schedule.dueCapabilities(address: "lan:a", nowMs: 1_000))
    }

    /// The field case, in one test: the removal happened *after* the HELLO2
    /// that used to be the notice's only trigger. Edge-triggered, the removed
    /// phone never hears; level-triggered, it hears on the next re-offer.
    func testARemovalAfterTheMeetingStillReachesTheLinkThatIsAlreadyUp() {
        let schedule = OwnRosterNoticeSchedule()
        let met: Int64 = 10_000
        schedule.noteHello2(address: "lan:a", capabilities: capable)
        // The one offer the shipped build made, at the meeting.
        XCTAssertEqual(schedule.dueCapabilities(address: "lan:a", nowMs: met), capable)
        schedule.noteOffered(address: "lan:a", nowMs: met)

        // The person removes the other device a few seconds later. Nothing on
        // this link changes: no new HELLO, no new capability exchange.
        XCTAssertNil(schedule.dueCapabilities(address: "lan:a", nowMs: met + 5_000))

        // The re-offer is what carries the news, and it is due on core's
        // cadence rather than on any event this phone has to have seen.
        XCTAssertEqual(
            schedule.dueCapabilities(address: "lan:a", nowMs: met + reofferIntervalMs),
            capable
        )
    }

    func testAPhoneThatCannotReadANoticeIsNeverSentOne() {
        let schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2(address: "lan:a", capabilities: 0)
        XCTAssertNil(schedule.dueCapabilities(address: "lan:a", nowMs: 1_000))
        XCTAssertNil(schedule.dueCapabilities(address: "lan:a", nowMs: reofferIntervalMs * 10))
    }

    func testAClosedLinkIsOwedNothingAndANewOneStartsOwedAgain() {
        let schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2(address: "lan:a", capabilities: capable)
        schedule.noteOffered(address: "lan:a", nowMs: 1_000)
        schedule.forget(address: "lan:a")
        XCTAssertNil(schedule.dueCapabilities(address: "lan:a", nowMs: 2_000))

        schedule.noteHello2(address: "lan:b", capabilities: capable)
        XCTAssertEqual(schedule.dueCapabilities(address: "lan:b", nowMs: 2_000), capable)
        schedule.clear()
        XCTAssertNil(schedule.dueCapabilities(address: "lan:b", nowMs: 3_000))
    }

    func testASecondHelloOnALiveLinkDoesNotResetItsCadence() {
        let schedule = OwnRosterNoticeSchedule()
        schedule.noteHello2(address: "lan:a", capabilities: capable)
        schedule.noteOffered(address: "lan:a", nowMs: 1_000)
        // A re-sent HELLO2 (a reconnect racing the reader, a peer repeating
        // itself) must not turn the timer into a per-HELLO spray.
        schedule.noteHello2(address: "lan:a", capabilities: capable)
        XCTAssertNil(schedule.dueCapabilities(address: "lan:a", nowMs: 1_500))
    }
}
