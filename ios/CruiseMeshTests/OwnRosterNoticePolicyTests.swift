import XCTest
@testable import CruiseMesh

/// Who may be handed this person's own device list (`specs/multi-device-v1.md`
/// §10 step 5).
///
/// The frame is plaintext on the link, so this predicate is the whole of its
/// safety — a stranger who merely *claims* our user id in a HELLO must not be
/// able to ask how many devices we have, and must not be able to hand us a
/// document to act on. Both directions run through `OwnRosterNoticePolicy`, so
/// both are pinned here.
///
/// The Swift twin of Android's `OwnRosterNoticePolicyTest`.
final class OwnRosterNoticePolicyTests: XCTestCase {
    private let ownAgreePk = Data(repeating: 0x11, count: 32)
    private let someoneElse = Data(repeating: 0x22, count: 32)

    func testALinkThatProvedItHoldsOurOwnKeyMayCarryOne() {
        XCTAssertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: ownAgreePk
            )
        )
    }

    func testAStrangerOnTheLanMayNotHoweverItsHelloIsAddressed() {
        XCTAssertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: someoneElse
            )
        )
    }

    func testAnUnauthenticatedLanLinkMayNot() {
        XCTAssertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: nil
            )
        )
    }

    /// The BLE limitation, stated as a test rather than left to be rediscovered:
    /// a BLE HELLO is cleartext and carries no proof at all, so a removed device
    /// that only ever meets its fleet over Bluetooth does not converge. The spec
    /// records it; weakening this line is not the fix.
    func testABleLinkNeverMayBecauseItProvesNothing() {
        XCTAssertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: false,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: ownAgreePk
            )
        )
    }

    func testAPeerThatNeverAdvertisedTheFrameIsNotSentOne() {
        XCTAssertFalse(OwnRosterNoticePolicy.peerReadsNotices(peerCapabilities: 0))
        XCTAssertTrue(
            OwnRosterNoticePolicy.peerReadsNotices(
                peerCapabilities: OwnRosterNoticePolicy.capabilityBit
            )
        )
    }

    /// The bit is a number in two places — `CAP_OWN_ROSTER_NOTICE` in core and
    /// the copy this shell tests peers against — because a capability mask does
    /// not cross the binding. This is the tripwire that stops the copy drifting:
    /// our own advertisement is built by core, so if the bit moved there and not
    /// here, this fails.
    func testTheBitThisShellLooksForIsTheOneCoreAdvertises() {
        XCTAssertEqual(OwnRosterNoticePolicy.capabilityBit, 1 << 4)
        XCTAssertTrue(
            OwnRosterNoticePolicy.peerReadsNotices(peerCapabilities: coreOwnCapabilities())
        )
    }
}
