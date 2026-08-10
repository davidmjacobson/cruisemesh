import XCTest
@testable import CruiseMesh

final class RemotePushRegistrationClientTests: XCTestCase {
    func testRegistrationUsesMemberAuthAndDeduplicatedSaltedHints() throws {
        let config = RelayConfig(relayUrl: "https://relay.example/", relayToken: "member-secret")
        let hint = Data(repeating: 7, count: 16)
        let request = try XCTUnwrap(RemotePushRegistrationClient.buildRequest(
            config: config,
            deviceToken: String(repeating: "ab", count: 32),
            hints: [hint, hint]
        ))

        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertEqual(request.url?.absoluteString, "https://relay.example/push/registrations")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer member-secret")
        let payload = try JSONDecoder().decode(
            RemotePushRegistrationPayload.self,
            from: try XCTUnwrap(request.httpBody)
        )
        XCTAssertEqual(payload.deviceToken, String(repeating: "ab", count: 32))
        XCTAssertEqual(payload.hints.count, 1)
    }

    func testRegistrationRejectsInsecureRelayURL() {
        XCTAssertNil(RemotePushRegistrationClient.buildRequest(
            config: RelayConfig(relayUrl: "http://relay.example", relayToken: "member-secret"),
            deviceToken: String(repeating: "ab", count: 32),
            hints: [Data(repeating: 7, count: 16)]
        ))
    }
}
