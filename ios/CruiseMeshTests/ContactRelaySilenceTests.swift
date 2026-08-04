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

    // MARK: - the pass-local skip

    func testAnAddressThatJustFailedIsNotDialledAgainForTheRestOfThePass() {
        // The whole point of the pass-local arm. A rest needs two passes, so
        // before this the first failure taught the pass nothing and a backlog
        // of queued envelopes re-dialled the same dead host once each -- 352
        // TLS handshakes in 27 seconds in the field report this came from.
        let silence = ContactRelaySilence.shared
        silence.beginPass()
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now), "never tried yet")
        XCTAssertTrue(
            silence.noteUnreachableThisPass(userId: alice, endpointKey: dead),
            "first failure is news"
        )
        XCTAssertFalse(
            silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now),
            "every later envelope this pass skips it"
        )
    }

    func testOnlyTheFirstFailurePerAddressInAPassIsWorthLogging() {
        let silence = ContactRelaySilence.shared
        silence.beginPass()
        XCTAssertTrue(silence.noteUnreachableThisPass(userId: alice, endpointKey: dead))
        XCTAssertFalse(
            silence.noteUnreachableThisPass(userId: alice, endpointKey: dead),
            "same address again says nothing new"
        )
        XCTAssertTrue(
            silence.noteUnreachableThisPass(userId: alice, endpointKey: live),
            "a different address is its own news"
        )
    }

    func testACardThatMovesTheContactMidPassIsTriedImmediately() {
        // Same rule as the rest window: a host that has never been dialled
        // cannot have been silent, so a T23 notice or a fresh card arriving
        // between two envelopes must not serve out the old address's skip.
        let silence = ContactRelaySilence.shared
        silence.beginPass()
        silence.noteUnreachableThisPass(userId: alice, endpointKey: dead)
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: live, nowMs: now))
    }

    func testThePassLocalSkipDoesNotSurviveIntoTheNextPass() {
        // It is not a rest and must not act like one: one failed pass is
        // explicitly not enough to write an endpoint off, so the next pass
        // owes it a fresh probe.
        let silence = ContactRelaySilence.shared
        silence.beginPass()
        silence.noteUnreachableThisPass(userId: alice, endpointKey: dead)
        let rested = silence.commitPass(otherRelayAnswered: true, nowMs: now)
        XCTAssertEqual(rested.count, 1)
        XCTAssertEqual(rested.first?.streak, 1)
        silence.beginPass()
        XCTAssertTrue(
            silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now),
            "one silent pass is still not enough"
        )
    }

    func testTwoSilentPassesStillRestTheEndpoint() {
        // The pass-local arm must not change what the streak means: this is
        // the pre-existing two-pass behaviour, now driven through commitPass.
        let silence = ContactRelaySilence.shared
        for expected in Int64(1)...2 {
            silence.beginPass()
            silence.noteUnreachableThisPass(userId: alice, endpointKey: dead)
            XCTAssertEqual(silence.commitPass(otherRelayAnswered: true, nowMs: now).first?.streak, expected)
        }
        silence.beginPass()
        XCTAssertFalse(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
    }

    func testSilenceCommittedWithoutProofOfWorkingInternetRestsNobody() {
        // A phone in a tunnel fails every endpoint at once. The pass-local
        // skip still saves the redundant dials inside that pass, but it must
        // not harden into a rest that takes the relay path away from the whole
        // contact list once connectivity returns.
        let silence = ContactRelaySilence.shared
        silence.beginPass()
        silence.noteUnreachableThisPass(userId: alice, endpointKey: dead)
        XCTAssertTrue(silence.commitPass(otherRelayAnswered: false, nowMs: now).isEmpty)
        silence.beginPass()
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
    }

    func testAnEndpointThatAnswersLaterInThePassIsDialledAgain() {
        // Recorded silence is provisional until the pass ends, so a success
        // against the same address -- a host that was mid-reboot -- has to
        // clear it outright rather than leave the rest of the pass skipping.
        let silence = ContactRelaySilence.shared
        silence.beginPass()
        silence.noteUnreachableThisPass(userId: alice, endpointKey: dead)
        silence.noteAnswered(userId: alice)
        XCTAssertTrue(silence.endpointAnswering(userId: alice, endpointKey: dead, nowMs: now))
        XCTAssertTrue(
            silence.commitPass(otherRelayAnswered: true, nowMs: now).isEmpty,
            "and nothing is left to commit"
        )
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
