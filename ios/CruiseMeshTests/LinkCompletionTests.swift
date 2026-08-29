import XCTest
@testable import CruiseMesh

/// One "Done" button, two endings, two destinations.
///
/// The field session on 2026-08-18 hit the first row of this table: a finished
/// adoption went back the way it came and landed on the first-run wizard, which
/// offered the link door again and then asked a linked person their own name.
/// The rows below it are the ones a naive fix breaks — the approving device
/// belongs back in "Your devices", and a run that failed is not a phone that is
/// set up, which on this shell is exactly what used to be recorded.
///
/// The Swift twin of Android's `LinkCompletionTest`.
final class LinkCompletionTests: XCTestCase {

    func testAnAdoptedPhoneGoesIntoTheApp() {
        XCTAssertTrue(LinkCompletion.entersApp(role: .newDevice, step: .done))
    }

    func testARunThatFailedIsNotAPhoneThatIsSetUp() {
        XCTAssertFalse(LinkCompletion.entersApp(role: .newDevice, step: .failed))
    }

    func testThePhoneThatDidTheAdoptingGoesBackWhereItCameFrom() {
        XCTAssertFalse(LinkCompletion.entersApp(role: .approvingDevice, step: .done))
        XCTAssertFalse(LinkCompletion.entersApp(role: .approvingDevice, step: .failed))
    }

    /// Listed rather than iterated: `LinkStep` is this shell's own enum and a
    /// step added to it should fail here as a missing line rather than pass by
    /// being silently included.
    func testNothingMidRunCountsAsAnEnding() {
        for step: LinkStep in [
            .idle, .waitingForPeer, .handshaking, .comparingDigits,
            .carryingBootstrap, .activating,
        ] {
            XCTAssertFalse(
                LinkCompletion.entersApp(role: .newDevice, step: step),
                "step \(step) is mid-run and must not enter the app"
            )
            XCTAssertFalse(LinkCompletion.entersApp(role: .approvingDevice, step: step))
        }
    }

    /// The regression this pair of rules exists for: a cancelled run kept the
    /// code and the copy button on screen, so the screen that said "Stopped"
    /// was at the same time inviting somebody to scan something dead.
    func testAStoppedRunShowsNoCodeToScan() {
        XCTAssertFalse(LinkCompletion.showsOffer(role: .newDevice, step: .failed))
    }

    func testAFinishedRunShowsNoCodeEither() {
        XCTAssertFalse(LinkCompletion.showsOffer(role: .newDevice, step: .done))
    }

    func testALiveRunStillShowsItsCode() {
        for step: LinkStep in [
            .idle, .waitingForPeer, .handshaking, .comparingDigits,
            .carryingBootstrap, .activating,
        ] {
            XCTAssertTrue(
                LinkCompletion.showsOffer(role: .newDevice, step: step),
                "step \(step) is live and still needs its code"
            )
        }
    }

    /// The approving end scans an offer; it never shows one.
    func testTheApprovingDeviceNeverShowsACode() {
        for step: LinkStep in [
            .idle, .waitingForPeer, .handshaking, .comparingDigits,
            .carryingBootstrap, .activating, .done, .failed,
        ] {
            XCTAssertFalse(LinkCompletion.showsOffer(role: .approvingDevice, step: step))
        }
    }

    func testOnlyAStoppedRunOffersAnotherGo() {
        XCTAssertTrue(LinkCompletion.offersRestart(step: .failed))
        for step: LinkStep in [
            .idle, .waitingForPeer, .handshaking, .comparingDigits,
            .carryingBootstrap, .activating, .done,
        ] {
            XCTAssertFalse(LinkCompletion.offersRestart(step: step))
        }
    }
}
