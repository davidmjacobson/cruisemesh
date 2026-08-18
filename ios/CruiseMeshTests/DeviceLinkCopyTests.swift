import XCTest
@testable import CruiseMesh

/// Every ending, step and refusal the device journeys can reach has words, and
/// none of those words is protocol jargon (`specs/multi-device-v1.md` §13's
/// product bar: obvious for a family on the surface).
///
/// The Swift twin of Android's `AddDeviceCopyTest`. Its real job is the negative
/// one: a new `CoreLinkOutcome`, `LinkStep` or refusal added to the core will
/// fail to compile here rather than reach a phone as a blank line.
final class DeviceLinkCopyTests: XCTestCase {
    private let jargon = ["roster", "epoch", "tombstone", "certificate"]

    private func assertFamilyWords(_ text: String, _ what: String, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertFalse(text.isEmpty, "\(what) has no words", file: file, line: line)
        for word in jargon {
            XCTAssertFalse(
                text.lowercased().contains(word),
                "\(what) leaked protocol jargon: \(word) — \(text)",
                file: file,
                line: line
            )
        }
    }

    func testEveryCeremonyStepHasFamilyWords() {
        for step: LinkStep in [
            .idle, .waitingForPeer, .handshaking, .comparingDigits,
            .carryingBootstrap, .activating, .done, .failed,
        ] {
            assertFamilyWords(stepText(step), "step \(step)")
        }
    }

    /// `channelReady` deliberately has no line of its own: the run kept going
    /// past it, and the counts the screen shows are what happened next.
    func testEveryEndingTheCoreNamesHasWordsExceptTheOneThatIsNotAnEnding() {
        XCTAssertNil(outcomeText(.channelReady))
        XCTAssertNil(outcomeText(nil))
        for outcome: CoreLinkOutcome in [
            .declined, .cancelled, .timedOut, .qrExpired,
            .deviceCapReached, .handshakeFailed, .protocolError,
        ] {
            let text = outcomeText(outcome)
            XCTAssertNotNil(text, "outcome \(outcome) has no words")
            assertFamilyWords(text ?? "", "outcome \(outcome)")
        }
    }

    func testEveryImportRefusalHasFamilyWords() {
        for readiness: CoreLinkImportReadiness in [.ready, .storeHoldsSomeone, .storeHoldsAnotherPerson] {
            assertFamilyWords(readinessText(readiness), "readiness \(readiness)")
        }
    }

    func testEveryRemovalRefusalHasFamilyWords() {
        for reason: RemoveDeviceRefusal in [
            .noDevices, .notTheApprovingDevice, .inboxKeyMissing,
            .noDeviceKeys, .earlierRemovalUnfinished, .coreRefused,
        ] {
            assertFamilyWords(removeRefusalText(reason), "refusal \(reason)")
        }
    }

    func testEveryReasonRemoveIsWithheldHasFamilyWords() {
        for block: RemoveDeviceBlock in [
            .notTheApprovingDevice, .isTheApprovingDevice, .lastDevice,
        ] {
            assertFamilyWords(
                removeBlockText(block, approverName: "Kitchen phone"),
                "block \(block)"
            )
        }
        // The one that needs more than a rule stated back: it names the device
        // that can do it, and the way out when that device is the one that is
        // gone -- which is why a person came looking for Remove at all.
        let wrongPhone = removeBlockText(.notTheApprovingDevice, approverName: "Kitchen phone")
        XCTAssertTrue(wrongPhone.contains("Kitchen phone"))
        XCTAssertTrue(wrongPhone.contains("contact support"))
        assertFamilyWords(
            addDeviceWithheldText(approverName: "Kitchen phone"),
            "add-device withheld"
        )
    }

    func testEveryDeviceLabelHasWords() {
        for label: DeviceLabel in [.numbered(position: 2), .removed, .unknown] {
            assertFamilyWords(deviceLabelText(label), "label \(label)")
        }
    }

    /// A family with no Shore Pass must not be told they are losing one.
    ///
    /// This half of the pin is unchanged by §10.2 landing: the rotation only
    /// exists where there is a credential to rotate, so on an install with no
    /// pass the confirmation still promises exactly what §10.1 and step 5 do.
    func testTheRemovalConfirmationPromisesNothingAboutAPassThatDoesNotExist() {
        let text = removeDeviceConfirmationText(deviceName: "Old iPad", hasShorePass: false)
        XCTAssertTrue(text.contains("Old iPad"))
        // It must say all three things §10.1 actually produces.
        XCTAssertTrue(text.contains("stops staying in step with your other devices"))
        XCTAssertTrue(text.contains("stay on your other devices"))
        XCTAssertTrue(text.contains("cannot undo this"))
        // And §10 step 5: the removed phone goes quiet when the two meet, not
        // at the moment of the tap. Saying otherwise is the promise this
        // confirmation made before there was anything behind it.
        XCTAssertTrue(text.contains("same Wi-Fi"))
        for overclaim in ["Shore Pass", "mailbox", "internet delivery"] {
            XCTAssertFalse(text.contains(overclaim), "the confirmation promised \(overclaim)")
        }
    }

    /// With a pass, the confirmation says the mailbox goes too — and says *when*.
    ///
    /// The timing word is the whole point of this test. §10.2's rotation is
    /// performed by the next relay pass that can reach the relay, not by the
    /// tap, so a removal made at sea leaves the removed phone holding a working
    /// credential until the phone finds internet. "As soon as this phone is
    /// online" is the honest version of that; "now" would not be.
    func testTheRemovalConfirmationSaysTheMailboxGoesWhenThePhoneIsOnline() {
        let text = removeDeviceConfirmationText(deviceName: "Old iPad", hasShorePass: true)
        XCTAssertTrue(text.contains("Old iPad"))
        XCTAssertTrue(text.contains("stops staying in step with your other devices"))
        XCTAssertTrue(text.contains("Shore Pass mailbox"))
        XCTAssertTrue(text.contains("as soon as this phone is online"))
        XCTAssertTrue(text.contains("same Wi-Fi"))
        XCTAssertTrue(text.contains("cannot undo this"))
    }
}
