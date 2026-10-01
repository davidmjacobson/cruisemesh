import XCTest
@testable import CruiseMesh

/// The renewal path's shell-side rules: what the app links to, and when it
/// says anything at all.
///
/// The date rule itself is core's (`relay_pass_delivery_through_ms`, pinned in
/// `core/src/relay_wire.rs`); what is this shell's to prove is that a
/// not-read-yet status is the same silence as no end date, and that the link
/// the app hands the browser is the one the site can resolve. Mirrors Android
/// `ShorePassRenewalTest`.
///
/// The fragment is the part worth pinning: a family token in a query string
/// would reach the server, its access log, and every hop in between, which is
/// exactly what the setup-card and friend-card links avoid by travelling the
/// same way.
final class ShorePassRenewalTests: XCTestCase {
    private let now: Int64 = 1_800_000_000_000
    private let memberToken = "b4c1f0a95e2d47318af6c0d21e7b9a83"

    private func status(
        expiresMs: Int64?,
        state: CoreFamilyPassState = .active
    ) -> CoreFamilyStatus {
        CoreFamilyStatus(plan: "shore", expiresMs: expiresMs, state: state)
    }

    func testTokenTravelsInTheFragmentAndNeverTheQuery() throws {
        let url = try XCTUnwrap(ShorePassRenewal.renewURL(familyToken: memberToken))
        XCTAssertEqual(url.absoluteString, "https://cruisemesh.app/renew/app#f=\(memberToken)")
        XCTAssertEqual(url.fragment, "f=\(memberToken)")
        // The invariant this whole shape exists for: a fragment is never sent
        // to a server, so the credential stays out of every log on the way.
        XCTAssertNil(url.query)
    }

    func testSurroundingWhitespaceNeverReachesTheLink() {
        XCTAssertEqual(
            ShorePassRenewal.renewURL(familyToken: "  \(memberToken)\n"),
            ShorePassRenewal.renewURL(familyToken: memberToken)
        )
    }

    func testThereIsNoLinkWhenThereIsNoTokenTheSiteCouldResolve() {
        XCTAssertNil(ShorePassRenewal.renewURL(familyToken: ""))
        XCTAssertNil(ShorePassRenewal.renewURL(familyToken: "   "))
        // A deposit credential is the post-only attenuation friend cards
        // carry, not the family token a purchase row is keyed by.
        XCTAssertNil(
            ShorePassRenewal.renewURL(familyToken: relayDepositTokenFor(memberToken: memberToken))
        )
        // Anything that would have to be escaped is refused rather than
        // encoded: one place for the app and the site to disagree about what
        // the token was is one too many, and a token allowed to carry a `#` or
        // an `&` could shape the link rather than ride it.
        XCTAssertNil(ShorePassRenewal.renewURL(familyToken: "token with spaces"))
        XCTAssertNil(ShorePassRenewal.renewURL(familyToken: "token#f=other"))
        XCTAssertNil(ShorePassRenewal.renewURL(familyToken: "token&next=evil"))
    }

    func testAnUnreadStatusSaysExactlyWhatAPassWithNoEndDateSays() {
        XCTAssertNil(ShorePassRenewal.deliveryThroughMs(status: nil, nowMs: now))
        XCTAssertNil(ShorePassRenewal.deliveryThroughMs(status: status(expiresMs: nil), nowMs: now))
    }

    func testAFutureEndDateIsShownAndAPastOneIsNot() {
        XCTAssertEqual(
            ShorePassRenewal.deliveryThroughMs(status: status(expiresMs: now + 1), nowMs: now),
            now + 1
        )
        XCTAssertNil(
            ShorePassRenewal.deliveryThroughMs(
                status: status(expiresMs: now - 1, state: .grace),
                nowMs: now
            )
        )
        // A suspended pass makes no delivery claim, whatever date it carries.
        XCTAssertNil(
            ShorePassRenewal.deliveryThroughMs(
                status: status(expiresMs: now + 1, state: .suspended),
                nowMs: now
            )
        )
    }

    func testAStateThisBuildCannotPlaceStillShowsItsDate() {
        // Core's forward-compatibility rule, from the shell's side: the reader
        // came for the date, and a state word this build has no rule for is no
        // reason to withhold one the server stated plainly.
        XCTAssertEqual(
            ShorePassRenewal.deliveryThroughMs(
                status: status(expiresMs: now + 1, state: .unknown),
                nowMs: now
            ),
            now + 1
        )
    }

    func testRenewalIsOfferedWhileADateIsStillAheadAndOnceItHasRunOut() {
        XCTAssertTrue(
            ShorePassRenewal.offersRenewal(health: .ok(lastSyncMs: now), deliveryThroughMs: now + 1)
        )
        XCTAssertTrue(
            ShorePassRenewal.offersRenewal(health: .expired(lastAttemptMs: now), deliveryThroughMs: nil)
        )
    }

    func testRenewalIsNotOfferedWherePayingAgainWouldNotHelp() {
        // No end date: nothing to renew. This is the self-hosted relay, and
        // the phone that has simply not read its status yet.
        XCTAssertFalse(
            ShorePassRenewal.offersRenewal(health: .ok(lastSyncMs: now), deliveryThroughMs: nil)
        )
        // A suspension is not lifted by paying, and a rejected setup card is a
        // different problem with its own instructions.
        XCTAssertFalse(
            ShorePassRenewal.offersRenewal(health: .suspended(lastAttemptMs: now), deliveryThroughMs: nil)
        )
        XCTAssertFalse(
            ShorePassRenewal.offersRenewal(
                health: .tokenRejected(lastAttemptMs: now),
                deliveryThroughMs: nil
            )
        )
        // Being offline is never a reason to sell someone anything.
        XCTAssertFalse(ShorePassRenewal.offersRenewal(health: .noInternet, deliveryThroughMs: nil))
    }
}
