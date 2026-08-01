import XCTest
@testable import CruiseMesh

/// Pins the Report action's two branches. The no-mail-app branch is the one
/// that matters: App Store Guideline 1.2 requires a working way to report
/// offensive content, and a phone with no configured mail account -- a
/// reviewer's device, typically -- used to get a button that did nothing at
/// all. Android has always handled this (`ui/ReportContact.kt`,
/// `ui_no_email_app`); these tests keep iOS at parity.
final class ReportContactTests: XCTestCase {
    private func contact() -> Contact {
        Contact(
            userId: Data(repeating: 0x11, count: 16),
            name: "Reported Peer",
            signPk: Data(repeating: 0x22, count: 32),
            agreePk: Data(repeating: 0x33, count: 32),
            relayUrl: nil,
            relayToken: nil
        )
    }

    private let reporter = Data(repeating: 0x44, count: 16)

    func testOpensMailWhenAMailAppExists() {
        let action = contactReportAction(
            contact: contact(),
            reporterUserId: reporter,
            canOpen: { _ in true }
        )
        guard case .openMail(let url) = action else {
            return XCTFail("expected .openMail, got \(action)")
        }
        XCTAssertEqual(url.scheme, "mailto")
        XCTAssertTrue(
            url.absoluteString.contains(abuseReportAddress),
            "the draft must be addressed to the published abuse contact"
        )
    }

    func testFallsBackToTheAddressWhenThereIsNoMailApp() {
        // The regression this file exists for. Before the fallback, this
        // branch called UIApplication.open on a URL nothing could handle and
        // the Report button silently did nothing.
        let action = contactReportAction(
            contact: contact(),
            reporterUserId: reporter,
            canOpen: { _ in false }
        )
        XCTAssertEqual(action, .showAddress(abuseReportAddress))
    }

    func testTheDraftCarriesWhatAModeratorNeedsToActOn() {
        // No message content is ever attached -- end-to-end encryption means
        // there is no server-side copy to include, so the report carries
        // identities and the reporter writes the rest.
        let action = contactReportAction(
            contact: contact(),
            reporterUserId: reporter,
            canOpen: { _ in true }
        )
        guard case .openMail(let url) = action else {
            return XCTFail("expected .openMail, got \(action)")
        }
        let decoded = url.absoluteString.removingPercentEncoding ?? url.absoluteString
        XCTAssertTrue(decoded.contains("Reported Peer"), "reported display name")
        XCTAssertTrue(decoded.contains(formatUserId(userId: contact().userId)), "reported id")
        XCTAssertTrue(decoded.contains(formatUserId(userId: reporter)), "reporter id")
        XCTAssertTrue(decoded.contains("CruiseMesh abuse report"), "subject")
    }

    func testTheNoMailMessageNamesTheAddress() {
        let message = noMailAppMessage(address: abuseReportAddress)
        XCTAssertTrue(message.contains(abuseReportAddress))
    }
}
