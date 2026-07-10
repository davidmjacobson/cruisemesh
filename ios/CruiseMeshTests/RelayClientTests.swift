import XCTest
@testable import CruiseMesh

/// URLProtocol mock that serves canned relay HTTP responses.
private final class RelayMockURLProtocol: URLProtocol {
    struct CannedResponse {
        let statusCode: Int
        let body: Data
        let headers: [String: String]
    }

    static var responses: [CannedResponse] = []
    static var requests: [URLRequest] = []

    static func reset() {
        responses = []
        requests = []
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.requests.append(request)
        guard !Self.responses.isEmpty else {
            client?.urlProtocol(self, didFailWithError: NSError(domain: "RelayMock", code: 1))
            return
        }
        let canned = Self.responses.removeFirst()
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: canned.statusCode,
            httpVersion: "HTTP/1.1",
            headerFields: canned.headers
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: canned.body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

final class RelayClientTests: XCTestCase {
    private var previousSession: URLSession!

    override func setUp() {
        super.setUp()
        previousSession = RelayClient.urlSession
        RelayMockURLProtocol.reset()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [RelayMockURLProtocol.self]
        RelayClient.urlSession = URLSession(configuration: config)
    }

    override func tearDown() {
        RelayClient.urlSession = previousSession
        RelayMockURLProtocol.reset()
        super.tearDown()
    }

    func testPostOutboundEnvelopeSendsBearerAuthAndPublicHeaderFields() throws {
        RelayMockURLProtocol.responses = [
            .init(statusCode: 200, body: Data(#"{"id":7}"#.utf8), headers: ["Content-Type": "application/json"]),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")
        let id = try RelayClient.postOutboundEnvelope(config: config, envelope: sampleOutboundEnvelope())
        XCTAssertEqual(id, 7)

        let request = try XCTUnwrap(RelayMockURLProtocol.requests.first)
        XCTAssertEqual(request.url?.path, "/envelopes")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer family-token")
        XCTAssertEqual(request.value(forHTTPHeaderField: "User-Agent"), "CruiseMeshRelayClient-iOS/0.1")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Bypass-Tunnel-Reminder"), "1")

        let json = try JSONSerialization.jsonObject(with: XCTUnwrap(request.httpBody)) as! [String: Any]
        XCTAssertEqual(json["msg_id"] as? String, base64Url(Data(repeating: 1, count: 16)))
        XCTAssertEqual((json["hop_ttl"] as? NSNumber)?.intValue, 7)
        XCTAssertEqual(json["recipient_hint"] as? String, base64Url(Data(repeating: 2, count: 8)))
        XCTAssertEqual(json["sealed"] as? String, base64Url(Data("sealed".utf8)))
        XCTAssertEqual((json["expiry_ms"] as? NSNumber)?.int64Value, 1_700_000_060_000)
    }

    func testPostReceiptEnvelopeUsesSameRelayContract() throws {
        RelayMockURLProtocol.responses = [
            .init(statusCode: 200, body: Data(#"{"id":11}"#.utf8), headers: [:]),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")
        let id = try RelayClient.postReceiptEnvelope(config: config, envelope: sampleReceiptEnvelope())
        XCTAssertEqual(id, 11)

        let request = try XCTUnwrap(RelayMockURLProtocol.requests.first)
        XCTAssertEqual(request.url?.path, "/envelopes")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer family-token")
        let json = try JSONSerialization.jsonObject(with: XCTUnwrap(request.httpBody)) as! [String: Any]
        XCTAssertEqual(json["msg_id"] as? String, base64Url(Data(repeating: 6, count: 16)))
        XCTAssertEqual((json["expiry_ms"] as? NSNumber)?.int64Value, 1_700_000_070_000)
    }

    func testFetchAndAckRoundTripRelayEnvelopeContract() throws {
        let msgIdB64 = base64Url(Data(repeating: 3, count: 16))
        let hintB64 = base64Url(Data(repeating: 4, count: 8))
        let sealedB64 = base64Url(Data("relay-sealed".utf8))
        let fetchBody = """
        {
          "envelopes": [
            {
              "id": 9,
              "msg_id": "\(msgIdB64)",
              "hop_ttl": 5,
              "recipient_hint": "\(hintB64)",
              "sealed": "\(sealedB64)",
              "expiry_ms": 1700009999999,
              "created_at_ms": 1700000000000
            }
          ],
          "next_cursor": 9
        }
        """
        RelayMockURLProtocol.responses = [
            .init(statusCode: 200, body: Data(fetchBody.utf8), headers: [:]),
            .init(statusCode: 200, body: Data(#"{"deleted":1}"#.utf8), headers: [:]),
        ]

        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")
        let page = try RelayClient.fetchEnvelopes(
            config: config,
            hints: [Data(repeating: 4, count: 8)],
            afterId: 0,
            limit: 50
        )
        XCTAssertEqual(page.envelopes.count, 1)
        XCTAssertEqual(page.nextCursor, 9)
        XCTAssertEqual(page.envelopes[0].id, 9)
        XCTAssertEqual(page.envelopes[0].hopTtl, 5)
        XCTAssertEqual(page.envelopes[0].msgId, Data(repeating: 3, count: 16))
        XCTAssertEqual(page.envelopes[0].sealed, Data("relay-sealed".utf8))

        try RelayClient.ackEnvelopes(config: config, ids: [9])

        XCTAssertEqual(RelayMockURLProtocol.requests.count, 2)
        let fetchRequest = RelayMockURLProtocol.requests[0]
        XCTAssertEqual(fetchRequest.httpMethod, "GET")
        XCTAssertTrue(fetchRequest.url?.absoluteString.contains("/envelopes?") == true)
        XCTAssertEqual(fetchRequest.value(forHTTPHeaderField: "User-Agent"), "CruiseMeshRelayClient-iOS/0.1")
        XCTAssertEqual(fetchRequest.value(forHTTPHeaderField: "Bypass-Tunnel-Reminder"), "1")

        let ackRequest = RelayMockURLProtocol.requests[1]
        XCTAssertEqual(ackRequest.url?.path, "/envelopes/ack")
        let ackJson = try JSONSerialization.jsonObject(with: XCTUnwrap(ackRequest.httpBody)) as! [String: Any]
        let ids = ackJson["ids"] as! [Any]
        XCTAssertEqual((ids[0] as? NSNumber)?.int64Value, 9)
    }

    func testFetchRejectsOutOfRangeHopTtlInsteadOfCrashing() {
        RelayMockURLProtocol.responses = [
            .init(
                statusCode: 200,
                body: Data(#"{"envelopes":[{"id":1,"msg_id":"AA","hop_ttl":999,"recipient_hint":"AA","sealed":"AA","expiry_ms":1700000000000}],"next_cursor":1}"#.utf8),
                headers: [:]
            ),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(try RelayClient.fetchEnvelopes(config: config, hints: [], afterId: 0, limit: 50))
    }

    func testPostRejectsMissingEnvelopeId() {
        RelayMockURLProtocol.responses = [
            .init(statusCode: 200, body: Data(#"{}"#.utf8), headers: [:]),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(try RelayClient.postOutboundEnvelope(config: config, envelope: sampleOutboundEnvelope()))
    }

    private func sampleOutboundEnvelope() -> OutboundEnvelope {
        OutboundEnvelope(
            msgId: Data(repeating: 1, count: 16),
            recipientUserId: Data(repeating: 9, count: 16),
            chatId: Data(repeating: 9, count: 16),
            senderUserId: Data(repeating: 8, count: 16),
            kind: 1,
            lamport: 1,
            timestamp: 1_700_000_000_000,
            hopTtl: 7,
            expiry: 1_700_000_060_000,
            recipientHint: Data(repeating: 2, count: 8),
            sealed: Data("sealed".utf8)
        )
    }

    private func sampleReceiptEnvelope() -> OutgoingReceiptEnvelope {
        OutgoingReceiptEnvelope(
            msgId: Data(repeating: 6, count: 16),
            recipientUserId: Data(repeating: 9, count: 16),
            chatId: Data(repeating: 9, count: 16),
            senderUserId: Data(repeating: 8, count: 16),
            receiptType: 2,
            throughLamport: 5,
            timestamp: 1_700_000_000_000,
            hopTtl: 7,
            expiry: 1_700_000_070_000,
            recipientHint: Data(repeating: 7, count: 8),
            sealed: Data("receipt-sealed".utf8)
        )
    }

    private func base64Url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
