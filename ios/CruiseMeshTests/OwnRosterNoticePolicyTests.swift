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
    /// A device id the transport reports having verified for this session.
    /// Sixteen bytes, as `core_derive_device_id` produces — the value itself is
    /// opaque here, because this predicate's job is to distinguish "proved" from
    /// "did not", not to re-verify what core already checked.
    private let siblingDeviceId = Data(repeating: 0x33, count: 16)
    private let removedDeviceId = Data(repeating: 0x44, count: 16)

    func testALinkThatProvedItHoldsOurOwnKeyMayCarryOne() {
        XCTAssertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: ownAgreePk,
                provenOwnDeviceId: nil
            )
        )
    }

    func testAStrangerOnTheLanMayNotHoweverItsHelloIsAddressed() {
        XCTAssertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: someoneElse,
                provenOwnDeviceId: nil
            )
        )
    }

    func testAnUnauthenticatedLanLinkMayNot() {
        XCTAssertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: nil,
                provenOwnDeviceId: nil
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
                sessionRemoteStaticKey: ownAgreePk,
                provenOwnDeviceId: nil
            )
        )
    }

    /// **The 2026-08-24 field case, pinned.**
    ///
    /// Two phones §9 linked as devices of one person hold *different* agreement
    /// keys: the ceremony gives the new device its own and withholds the person
    /// root secret. So the sibling's Noise static is not ours, and every
    /// agreement-key comparison on this path answers "stranger" — which is what
    /// refused the link 25 times across 15 minutes on one `/24` and left a
    /// removed phone believing it was still linked.
    ///
    /// What admits it is the roster proof the transport already verified for
    /// this session. Note the agreement keys deliberately do *not* match here:
    /// this case fails on the pre-fix predicate for exactly the reason the field
    /// did.
    func testASiblingThatSharesNoAgreementKeyMayCarryOne() {
        XCTAssertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: someoneElse,
                provenOwnDeviceId: siblingDeviceId
            )
        )
    }

    /// §10 step 5's whole purpose: the device that most needs the notice is the
    /// one that was removed. The transport admits a tombstoned device by design,
    /// and this predicate must not undo that — refusing here would slam the only
    /// door the notice can come through.
    func testARemovedSiblingMayStillBeToldWhichIsThePoint() {
        XCTAssertTrue(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: true,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: someoneElse,
                provenOwnDeviceId: removedDeviceId
            )
        )
    }

    /// A proven device id is still not a licence to skip the transport check: a
    /// BLE link carries no Noise session and therefore proves nothing, so the
    /// transport can never produce one there.
    func testABleLinkMayNotEvenHoldingADeviceId() {
        XCTAssertFalse(
            OwnRosterNoticePolicy.mayCross(
                isLanLink: false,
                ownAgreePk: ownAgreePk,
                sessionRemoteStaticKey: nil,
                provenOwnDeviceId: siblingDeviceId
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
