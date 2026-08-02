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
    static var requestBodies: [Data?] = []

    static func reset() {
        responses = []
        requests = []
        requestBodies = []
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.requests.append(request)
        Self.requestBodies.append(Self.readBody(from: request))
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

    private static func readBody(from request: URLRequest) -> Data? {
        if let body = request.httpBody {
            return body
        }
        guard let stream = request.httpBodyStream else {
            return nil
        }

        stream.open()
        defer { stream.close() }

        let bufferSize = 4_096
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: bufferSize)
        defer { buffer.deallocate() }

        var body = Data()
        while true {
            let bytesRead = stream.read(buffer, maxLength: bufferSize)
            if bytesRead < 0 {
                return nil
            }
            if bytesRead == 0 {
                return body
            }
            body.append(buffer, count: bytesRead)
        }
    }
}

final class RelayClientTests: XCTestCase {

    func testRelayURLNormalizationAddsHTTPSAndRemovesTrailingSlash() {
        XCTAssertEqual(normalizeRelayUrl(" relay.example/ "), "https://relay.example")
        XCTAssertEqual(normalizeRelayUrl("https://relay.example/"), "https://relay.example")
        XCTAssertEqual(normalizeRelayUrl("http://127.0.0.1:8080/"), "http://127.0.0.1:8080")
    }

    func testNonLoopbackHTTPRelayURLIsRefusedInsteadOfStored() {
        XCTAssertEqual(normalizeRelayUrl("http://relay.example"), "")
        XCTAssertEqual(normalizeRelayUrl("http://192.168.1.50:8080"), "")
        XCTAssertTrue(relayUrlIsInsecure(value: "http://relay.example"))
        XCTAssertFalse(relayUrlIsInsecure(value: "https://relay.example"))
        XCTAssertFalse(relayUrlIsInsecure(value: ""))
    }

    func testPostingToNonHTTPSRelayFailsWithLegibleError() {
        let config = RelayConfig(relayUrl: "http://relay.example", relayToken: "family-token")
        XCTAssertThrowsError(
            try RelayClient.postOutboundEnvelope(config: config, envelope: sampleOutboundEnvelope())
        ) { error in
            XCTAssertEqual((error as NSError).localizedDescription, "Relay URL must use https")
        }
    }

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

        let json = try JSONSerialization.jsonObject(
            with: XCTUnwrap(RelayMockURLProtocol.requestBodies[0])
        ) as! [String: Any]
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
        let json = try JSONSerialization.jsonObject(
            with: XCTUnwrap(RelayMockURLProtocol.requestBodies[0])
        ) as! [String: Any]
        XCTAssertEqual(json["msg_id"] as? String, base64Url(Data(repeating: 6, count: 16)))
        XCTAssertEqual((json["expiry_ms"] as? NSNumber)?.int64Value, 1_700_000_070_000)
    }

    func testHostedPassRejectionSurfacesStableRelayCode() {
        RelayMockURLProtocol.responses = [
            .init(
                statusCode: 403,
                body: Data(#"{"error":"relay pass expired","code":"family_expired"}"#.utf8),
                headers: ["Content-Type": "application/json"]
            ),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "expired-token")

        XCTAssertThrowsError(
            try RelayClient.syncPresence(config: config, announce: [], query: [])
        ) { error in
            let relay = error as? RelayHTTPError
            XCTAssertEqual(relay?.statusCode, 403)
            XCTAssertEqual(relay?.relayCode, "family_expired")
        }
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
            limit: 16
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
        let ackJson = try JSONSerialization.jsonObject(
            with: XCTUnwrap(RelayMockURLProtocol.requestBodies[1])
        ) as! [String: Any]
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

        XCTAssertThrowsError(try RelayClient.fetchEnvelopes(config: config, hints: [], afterId: 0, limit: 16))
    }

    func testPostRejectsMissingEnvelopeId() {
        RelayMockURLProtocol.responses = [
            .init(statusCode: 200, body: Data(#"{}"#.utf8), headers: [:]),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(try RelayClient.postOutboundEnvelope(config: config, envelope: sampleOutboundEnvelope()))
    }

    func testResponseContentLengthAboveCoreLimitIsRejectedBeforeBodyAccumulation() {
        RelayMockURLProtocol.responses = [
            .init(
                statusCode: 200,
                body: Data(#"{"id":7}"#.utf8),
                headers: ["Content-Length": "\(relayMaxResponseBytes() + 1)"]
            ),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(try RelayClient.postOutboundEnvelope(config: config, envelope: sampleOutboundEnvelope()))
    }

    func testAFetchPageTooBigToDecodeIsRetriedAtHalfTheLimitNotFailed() throws {
        // The stall: `limit` bounds rows, not bytes, so a mailbox of large
        // attachment chunks can produce a window past the response cap. The
        // next pass would ask the same relay for the same window from the
        // same cursor and fail identically -- the frontier never advances.
        // A self-hosted relay predating the server-side byte budget is
        // exactly this case, so the client must recover on its own.
        // Mirrors the Android RelayClientTest of the same name.
        RelayMockURLProtocol.responses = [
            .init(
                statusCode: 200,
                body: Data(#"{"envelopes":[],"next_cursor":0}"#.utf8),
                headers: ["Content-Length": "\(relayMaxResponseBytes() + 1)"]
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"envelopes":[],"next_cursor":42}"#.utf8),
                headers: [:]
            ),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        let fetched = try RelayClient.fetchEnvelopesWithinResponseCap(
            config: config,
            hints: [Data(repeating: 2, count: 8)],
            afterId: 42,
            limit: 256
        )

        XCTAssertEqual(fetched.limit, 128)
        XCTAssertEqual(fetched.page.nextCursor, 42)
        XCTAssertEqual(RelayMockURLProtocol.requests.count, 2)
        let first = RelayMockURLProtocol.requests[0].url!.absoluteString
        XCTAssertTrue(first.contains("limit=256"))
        XCTAssertTrue(first.contains("after=42"))
        // Same cursor, half the rows: nothing is skipped by recovering.
        let second = RelayMockURLProtocol.requests[1].url!.absoluteString
        XCTAssertTrue(second.contains("limit=128"))
        XCTAssertTrue(second.contains("after=42"))
    }

    func testAnOversizeSingleRowPageIsReportedRatherThanRetriedForever() {
        // Nothing smaller than one row can be asked for, so retrying would
        // just spin. Surface it instead.
        let oversize = RelayMockURLProtocol.CannedResponse(
            statusCode: 200,
            body: Data(#"{"envelopes":[],"next_cursor":0}"#.utf8),
            headers: ["Content-Length": "\(relayMaxResponseBytes() + 1)"]
        )
        RelayMockURLProtocol.responses = [oversize, oversize]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(
            try RelayClient.fetchEnvelopesWithinResponseCap(
                config: config,
                hints: [Data(repeating: 2, count: 8)],
                afterId: 7,
                limit: 2
            )
        ) { error in
            XCTAssertTrue(error is RelayResponseTooLargeError)
        }
        XCTAssertEqual(RelayMockURLProtocol.requests.count, 2)
        XCTAssertTrue(RelayMockURLProtocol.requests[0].url!.absoluteString.contains("limit=2"))
        XCTAssertTrue(RelayMockURLProtocol.requests[1].url!.absoluteString.contains("limit=1"))
    }

    func testABodyThatStopsAfterTheHeadIsReportedAsAPageTooBigToTake() throws {
        // A full page is megabytes. On a ship's Wi-Fi the transfer can start
        // and then not finish in time, and nothing about that changes on the
        // next pass: the same cursor asks for the same window and stops in the
        // same place, so the frontier never advances and the mail behind it is
        // never delivered. The link is telling us the window is too big, which
        // is the same thing an undecodable page says, so it must arrive at the
        // caller as the same kind of failure and get the same answer.
        //
        // Driven through the delegate rather than through the mock protocol on
        // purpose: staging "head, then a body that stops" over URLSession means
        // reporting the failure in the same breath as the head, and the loading
        // system is free to drop the head callback when the task has already
        // failed. That is a race, not a behaviour, and it belongs in no test.
        let partial = Data(#"{"envelopes":"#.utf8)
        let delegate = makeResponseDelegate()
        let task = idleDataTask()

        delegate.urlSession(
            RelayClient.urlSession,
            dataTask: task,
            didReceive: httpResponse(statusCode: 200)
        ) { disposition in
            XCTAssertEqual(disposition, .allow)
        }
        delegate.urlSession(RelayClient.urlSession, dataTask: task, didReceive: partial)
        delegate.urlSession(RelayClient.urlSession, task: task, didCompleteWithError: URLError(.timedOut))

        switch try XCTUnwrap(delegate.result()) {
        case .success:
            XCTFail("a body that stopped part-way is not a complete page")
        case .failure(let error):
            let stalled = try XCTUnwrap(error as? RelayResponseStalledError)
            XCTAssertEqual(stalled.bytesReceived, partial.count)
            // Shares the type `fetchEnvelopesWithinResponseCap` retries on, so
            // the recovery proven by the oversize-page tests above covers this
            // failure too -- there is one catch, not two.
            XCTAssertTrue(error is RelayPageTooBigError)
            let oversize: Error = RelayResponseTooLargeError(maxBytes: 8)
            XCTAssertTrue(oversize is RelayPageTooBigError)
        }
    }

    func testATimeoutBeforeTheHeadStaysAnOrdinaryTransportFailure() throws {
        // Nothing came back at all, so there is no evidence the window was too
        // big -- shrinking would only make the next attempt at an unreachable
        // relay smaller.
        let delegate = makeResponseDelegate()

        delegate.urlSession(
            RelayClient.urlSession,
            task: idleDataTask(),
            didCompleteWithError: URLError(.timedOut)
        )

        switch try XCTUnwrap(delegate.result()) {
        case .success:
            XCTFail("a failed request is not a page")
        case .failure(let error):
            XCTAssertFalse(error is RelayPageTooBigError)
            XCTAssertEqual((error as? URLError)?.code, .timedOut)
        }
    }

    func testATimeoutBeforeAnyResponseIsNotTreatedAsAPageProblem() {
        // The same rule seen from the fetch walk: an unreachable relay is
        // reported once, and the shrink ladder is never entered on the strength
        // of a request that produced nothing.
        RelayMockURLProtocol.responses = []
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(
            try RelayClient.fetchEnvelopesWithinResponseCap(
                config: config,
                hints: [Data(repeating: 2, count: 8)],
                afterId: 1,
                limit: 64
            )
        ) { error in
            XCTAssertFalse(error is RelayPageTooBigError)
        }
        XCTAssertEqual(RelayMockURLProtocol.requests.count, 1)
    }

    func testAnErrorPageIsReportedByItsStatusEvenWhenItIsEnormous() {
        // A captive portal, a proxy notice or a gateway error page can be any
        // size. Judging size before status would call one an oversized *page*
        // and send the fetch down the whole shrink ladder -- eight more round
        // trips that were never going to succeed.
        RelayMockURLProtocol.responses = [
            .init(
                statusCode: 502,
                body: Data(String(repeating: "x", count: 64 * 1_024).utf8),
                headers: ["Content-Length": "\(relayMaxResponseBytes() + 1)"]
            ),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(
            try RelayClient.fetchEnvelopesWithinResponseCap(
                config: config,
                hints: [Data(repeating: 2, count: 8)],
                afterId: 5,
                limit: 256
            )
        ) { error in
            XCTAssertEqual((error as? RelayHTTPError)?.statusCode, 502)
            XCTAssertFalse(error is RelayPageTooBigError)
            // Only a preview of the page is kept, not all 64 KiB of it.
            XCTAssertLessThanOrEqual((error as? RelayHTTPError)?.responseBody.count ?? .max, 2_048)
        }
        // One request, not nine: the ladder was never entered.
        XCTAssertEqual(RelayMockURLProtocol.requests.count, 1)
    }

    func testARateLimitKeepsItsRetryAfterInsteadOfLookingLikeAnOversizePage() {
        RelayMockURLProtocol.responses = [
            .init(
                statusCode: 429,
                body: Data(#"{"error":"too many requests","code":"rate_limited"}"#.utf8),
                headers: [
                    "Retry-After": "42",
                    "Content-Length": "\(relayMaxResponseBytes() + 1)",
                ]
            ),
        ]
        let config = RelayConfig(relayUrl: "https://relay.test", relayToken: "family-token")

        XCTAssertThrowsError(
            try RelayClient.fetchEnvelopes(
                config: config,
                hints: [Data(repeating: 2, count: 8)],
                afterId: 0,
                limit: 16
            )
        ) { error in
            let relay = error as? RelayHTTPError
            XCTAssertEqual(relay?.statusCode, 429)
            XCTAssertEqual(relay?.relayCode, "rate_limited")
            // The back-off the relay asked for survives; classifying this as
            // an oversize page would have thrown it away.
            XCTAssertEqual(relay?.retryAfter, "42")
        }
    }

    /// A response accumulator configured exactly as `RelayClient` builds one.
    private func makeResponseDelegate() -> BoundedRelayResponseDelegate {
        BoundedRelayResponseDelegate(
            maxBytes: Int(relayMaxResponseBytes()),
            errorPreviewBytes: 2_048,
            semaphore: DispatchSemaphore(value: 0)
        )
    }

    /// A data task that is never resumed: the delegate callbacks take one as an
    /// argument, and nothing under test does anything with it beyond cancelling
    /// it, which on an unstarted task is a no-op.
    private func idleDataTask() -> URLSessionDataTask {
        RelayClient.urlSession.dataTask(with: URL(string: "https://relay.test/envelopes")!)
    }

    private func httpResponse(statusCode: Int, headers: [String: String] = [:]) -> HTTPURLResponse {
        HTTPURLResponse(
            url: URL(string: "https://relay.test/envelopes")!,
            statusCode: statusCode,
            httpVersion: "HTTP/1.1",
            headerFields: headers
        )!
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
