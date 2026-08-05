import XCTest
@testable import CruiseMesh

final class RelayDiagnosticAttributionTests: XCTestCase {
    func testRequestLabelNamesHostWithoutCredentialsQueryOrHints() throws {
        let url = try XCTUnwrap(URL(
            string: "https://card-user:card-secret@Relay.Example:8443/base/envelopes?hint=recipient-hint&token=query-token#fragment"
        ))
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("Bearer family-token", forHTTPHeaderField: "Authorization")

        let label = relayDiagnosticRequestLabel(request)

        XCTAssertEqual(label, "POST /base/envelopes host=relay.example")
        for secret in ["card-user", "card-secret", "8443", "recipient-hint", "query-token", "family-token"] {
            XCTAssertFalse(label.contains(secret), "label leaked \(secret)")
        }
    }

    func testRelayHostReturnsOnlyHostAndHandlesInvalidValues() {
        XCTAssertEqual(
            relayDiagnosticHost("https://user:secret@Relay.Example:9443/path?hint=abc#fragment"),
            "relay.example"
        )
        XCTAssertEqual(relayDiagnosticHost("not a URL"), "unknown")
        XCTAssertEqual(relayDiagnosticHost(nil as String?), "unknown")
    }

    func testContactIdMatchesAndroidLowercaseHexPolicy() {
        XCTAssertEqual(relayDiagnosticContactId(Data([0x00, 0x0A, 0xFF, 0x7B])), "000aff7b")
    }
}
