import Foundation

struct RelayFetchedEnvelope {
    let id: Int64
    let msgId: Data
    let hopTtl: UInt8
    let recipientHint: Data
    let sealed: Data
    let expiryMs: Int64
}

struct RelayFetchPage {
    let envelopes: [RelayFetchedEnvelope]
    let nextCursor: Int64
}

struct RelayPresencePage {
    let nowMs: Int64
    let presence: [CoreRelayPresence]
}

/// HTTPS client for `cruisemesh-relayd` (DESIGN.md §9). Mirrors Android `RelayClient`.
enum RelayClient {
    private static let connectTimeout: TimeInterval = 10
    private static let userAgent = "CruiseMeshRelayClient-iOS/0.1"

    /// Overridable for unit tests (URLProtocol / mock sessions).
    static var urlSession: URLSession = .shared

    static func postOutboundEnvelope(config: RelayConfig, envelope: OutboundEnvelope) throws -> Int64 {
        try postEnvelope(
            config: config,
            msgId: Data(envelope.msgId),
            hopTtl: envelope.hopTtl,
            recipientHint: Data(envelope.recipientHint),
            sealed: Data(envelope.sealed),
            expiryMs: envelope.expiry
        )
    }

    static func postCarriedEnvelope(config: RelayConfig, envelope: CarriedEnvelope) throws -> Int64 {
        try postEnvelope(
            config: config,
            msgId: Data(envelope.msgId),
            hopTtl: envelope.hopTtl,
            recipientHint: Data(envelope.recipientHint),
            sealed: Data(envelope.sealed),
            expiryMs: envelope.expiry
        )
    }

    /// Posts one per-member fan-out row of a group message
    /// (specs/group-relay-durability.md §4; built by the core's
    /// `coreGroupFanoutRows`/`coreGroupFanoutRowsForCarried`). Same wire
    /// shape as every other envelope post -- fan-out changes addressing,
    /// not format. Mirrors Android `RelayClient.postFanoutRow`.
    static func postFanoutRow(config: RelayConfig, row: CoreGroupFanoutRow) throws -> Int64 {
        try postEnvelope(
            config: config,
            msgId: Data(row.msgId),
            hopTtl: row.hopTtl,
            recipientHint: Data(row.recipientHint),
            sealed: Data(row.sealed),
            expiryMs: row.expiry
        )
    }

    static func postReceiptEnvelope(config: RelayConfig, envelope: OutgoingReceiptEnvelope) throws -> Int64 {
        try postEnvelope(
            config: config,
            msgId: Data(envelope.msgId),
            hopTtl: envelope.hopTtl,
            recipientHint: Data(envelope.recipientHint),
            sealed: Data(envelope.sealed),
            expiryMs: envelope.expiry
        )
    }

    static func fetchEnvelopes(config: RelayConfig, hints: [Data], afterId: Int64, limit: Int) throws -> RelayFetchPage {
        let path = try relayBuildFetchPath(hints: hints, afterId: afterId, limit: UInt32(limit))
        let url = try buildURL(config.relayUrl, path: path)
        var request = URLRequest(url: url, timeoutInterval: connectTimeout)
        request.httpMethod = "GET"
        applyAuth(&request, config: config)
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
        let page = try relayDecodeFetchPage(body: data)
        let envelopes: [RelayFetchedEnvelope] = page.envelopes.map { item in
            return RelayFetchedEnvelope(
                id: item.id, msgId: item.msgId, hopTtl: item.hopTtl,
                recipientHint: item.recipientHint, sealed: item.sealed, expiryMs: item.expiryMs
            )
        }
        return RelayFetchPage(envelopes: envelopes, nextCursor: page.nextCursor)
    }

    static func ackEnvelopes(config: RelayConfig, ids: [Int64]) throws {
        guard !ids.isEmpty else { return }
        let url = try buildURL(config.relayUrl, path: "/envelopes/ack")
        var request = URLRequest(url: url, timeoutInterval: connectTimeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        applyAuth(&request, config: config)
        request.httpBody = try relayEncodeAckRequest(ids: ids)
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
    }

    static func syncPresence(
        config: RelayConfig,
        announce: [Data],
        query: [Data]
    ) throws -> RelayPresencePage {
        let url = try buildURL(config.relayUrl, path: "/presence")
        var request = URLRequest(url: url, timeoutInterval: connectTimeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        applyAuth(&request, config: config)
        request.httpBody = try relayEncodePresenceRequest(announce: announce, query: query)
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
        let page = try relayDecodePresencePage(body: data)
        return RelayPresencePage(nowMs: page.nowMs, presence: page.presence)
    }

    private static func postEnvelope(
        config: RelayConfig,
        msgId: Data,
        hopTtl: UInt8,
        recipientHint: Data,
        sealed: Data,
        expiryMs: Int64
    ) throws -> Int64 {
        let url = try buildURL(config.relayUrl, path: "/envelopes")
        var request = URLRequest(url: url, timeoutInterval: connectTimeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        applyAuth(&request, config: config)
        request.httpBody = try relayEncodePostEnvelope(
            msgId: msgId, hopTtl: hopTtl, recipientHint: recipientHint,
            sealed: sealed, expiryMs: expiryMs
        )
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
        return try relayDecodePostResponse(body: data)
    }

    private static func applyAuth(_ request: inout URLRequest, config: RelayConfig) {
        request.setValue("Bearer \(config.relayToken)", forHTTPHeaderField: "Authorization")
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        request.setValue("1", forHTTPHeaderField: "Bypass-Tunnel-Reminder")
    }

    private static func buildURL(_ base: String, path: String) throws -> URL {
        guard let url = URL(string: normalizeRelayUrl(base) + path) else {
            throw NSError(domain: "RelayClient", code: 1, userInfo: [NSLocalizedDescriptionKey: "bad URL"])
        }
        return url
    }

    private static func syncRequest(_ request: URLRequest) throws -> (Data, URLResponse) {
        let sem = DispatchSemaphore(value: 0)
        var result: Result<(Data, URLResponse), Error>?
        let task = urlSession.dataTask(with: request) { data, response, error in
            if let error {
                result = .failure(error)
            } else if let data, let response {
                result = .success((data, response))
            } else {
                result = .failure(NSError(domain: "RelayClient", code: 2, userInfo: [NSLocalizedDescriptionKey: "empty response"]))
            }
            sem.signal()
        }
        task.resume()
        guard sem.wait(timeout: .now() + connectTimeout + 5) == .success else {
            task.cancel()
            throw URLError(.timedOut)
        }
        guard let result else {
            throw malformedResponse("request completed without a result")
        }
        return try result.get()
    }

    private static func ensureOK(_ response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse else {
            throw malformedResponse("non-HTTP relay response")
        }
        guard (200..<300).contains(http.statusCode) else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw NSError(
                domain: "RelayClient",
                code: http.statusCode,
                userInfo: [NSLocalizedDescriptionKey: "HTTP \(http.statusCode): \(body)"]
            )
        }
    }

    private static func malformedResponse(_ message: String) -> NSError {
        NSError(
            domain: "RelayClient",
            code: 4,
            userInfo: [NSLocalizedDescriptionKey: message]
        )
    }
}
