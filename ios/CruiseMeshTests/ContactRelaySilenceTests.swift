import XCTest
@testable import CruiseMesh

/// The two ways a contact's friend-card relay endpoint dies, and the one place
/// they must not be treated alike: the group fan-out's choice of mailbox.
///
/// Android twin: `ContactRelaySilenceTest.kt` and the group cases in
/// `ContactRelayHealthPolicyTest.kt`, case for case.
final class ContactRelaySilenceTests: XCTestCase {
    private let alice = Data("alice".utf8)
    private let dead = relayCursorKey(relayUrl: "https://dead.example", relayToken: "tok")
    private let live = relayCursorKey(relayUrl: "https://live.example", relayToken: "tok")
    private let now: Int64 = 1_800_000_000_000
    private let restWindow: Int64 = 30 * 60 * 1000

    override func setUp() {
        super.setUp()
        ContactRelaySilence.shared.reset()
    }

    override func tearDown() {
        ContactRelaySilence.shared.reset()
        super.tearDown()
    }

    private func rest() {
        let silence = ContactRelaySilence.shared
        XCTAssertEqual(silence.noteSilentPass(userId: alice, endpointKey: dead, otherRelayAnswered: true, nowMs: now), 1)
        XCTAssertEqual(silence.noteSilentPass(userId: alice, endpointKey: dead, otherRelayAnswered: true, nowMs: now), 2)
    }

    // MARK: - the silence state machine

    func testAnEndpointNobodyHasHeardFromIsRestedAfterTwoPasses() {
        let silence = ContactRelaySilence.shared
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
        silence.noteSilentPass(userId: alice, endpointKey: dead, otherRelayAnswered: true, nowMs: now)
        XCTAssertTrue(
            silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now),
            "one silent pass is not enough"
        )
        silence.noteSilentPass(userId: alice, endpointKey: dead, otherRelayAnswered: true, nowMs: now)
        XCTAssertFalse(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now), "two are")
    }

    func testSilenceWithoutProofOfWorkingInternetIsNotRecordedAtAll() {
        // The shell must not decide this for itself: the observation goes to
        // the core with the real value and the core returns a zero delta. A
        // phone in a tunnel fails every endpoint at once, and resting them all
        // would take the relay path away from the whole contact list.
        let silence = ContactRelaySilence.shared
        XCTAssertNil(silence.noteSilentPass(userId: alice, endpointKey: dead, otherRelayAnswered: false, nowMs: now))
        XCTAssertNil(silence.noteSilentPass(userId: alice, endpointKey: dead, otherRelayAnswered: false, nowMs: now))
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
    }

    func testARestedEndpointIsProbedAgainOnceTheWindowIsUp() {
        rest()
        let silence = ContactRelaySilence.shared
        XCTAssertFalse(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now + restWindow - 1))
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now + restWindow))
    }

    func testMovingTheContactToADifferentEndpointEndsTheRestImmediately() {
        // A new friend card or a T23 relay-update notice that changes the
        // address clears the persisted rejection streak in core; this is the
        // same rule for the unpersisted silence rest. Without it a contact who
        // migrated to a working host would keep being skipped for up to half
        // an hour after the repair arrived.
        rest()
        let silence = ContactRelaySilence.shared
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: live, nowMs: now))
        // And the stale verdict is genuinely gone, not merely bypassed.
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
    }

    func testReStatingTheSameEndpointKeepsTheRest() {
        // Re-sharing a card from a phone whose config never changed carries
        // the SAME dead endpoint. Clearing for that would restart the
        // hammering and make the repair look like it worked, exactly as core
        // refuses to launder a rejection streak for it.
        rest()
        XCTAssertFalse(ContactRelaySilence.shared.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
    }

    func testAnEndpointThatAnswersSettlesTheQuestionOutright() {
        rest()
        ContactRelaySilence.shared.noteAnswered(userId: alice)
        XCTAssertTrue(ContactRelaySilence.shared.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
    }

    // MARK: - the group fan-out's choice of mailbox

    private func member(_ url: String?, usable: Bool, answering: Bool) -> GroupRelayMember {
        GroupRelayMember(
            relayUrl: url,
            relayToken: "their-token",
            endpointUsable: usable,
            endpointAnswering: answering
        )
    }

    func testAGroupWhoseOnlyCardMemberIsRestingIsNotPostedAtAll() {
        // The 1:1 paths skip a resting endpoint; the group fan-out used to
        // fall through to our own mailbox instead. That post succeeds, the
        // envelope is marked relay-posted -- which is terminal -- and a
        // cross-family member's copy is stranded in a mailbox they never read,
        // with no later pass to repair it. Nil means "post nothing this pass",
        // leaving it queued for BLE/LAN and for a later pass.
        XCTAssertNil(coreGroupFanoutRelayTarget(
            members: [member("https://silent.example", usable: true, answering: false)],
            fallbackUrl: "https://ours.example",
            fallbackToken: "our-token"
        ))
    }

    func testAMemberWrittenOffForRejectionStillFallsBackToOurOwnMailbox() {
        // Deliberately not the same answer: a 401 proves the card is wrong,
        // and our own relay really delivers when both sides have since moved
        // to the same new host. Pinned so the resting fix above cannot quietly
        // take this path with it.
        let target = coreGroupFanoutRelayTarget(
            members: [member("https://revoked.example", usable: false, answering: true)],
            fallbackUrl: "https://ours.example",
            fallbackToken: "our-token"
        )
        XCTAssertEqual(target?.url, "https://ours.example")
    }

    func testAGroupWithAHealthyMemberStillRidesThatMembersRelay() {
        let target = coreGroupFanoutRelayTarget(
            members: [
                member("https://silent.example", usable: true, answering: false),
                member("https://live.example", usable: true, answering: true)
            ],
            fallbackUrl: "https://ours.example",
            fallbackToken: "our-token"
        )
        XCTAssertEqual(target?.url, "https://live.example")
    }
}
