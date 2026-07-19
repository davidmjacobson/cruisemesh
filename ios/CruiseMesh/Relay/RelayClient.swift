import Foundation

private final class BoundedRelayResponseDelegate: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let maxBytes: Int
    private let semaphore: DispatchSemaphore
    private let lock = NSLock()
    private var data = Data()
    private var response: URLResponse?
    private var completedResult: Result<(Data, URLResponse), Error>?

    init(maxBytes: Int, semaphore: DispatchSemaphore) {
        self.maxBytes = maxBytes
        self.semaphore = semaphore
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        if response.expectedContentLength > Int64(maxBytes) {
            finish(.failure(Self.tooLarge(maxBytes)))
            completionHandler(.cancel)
            return
        }
        self.response = response
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive chunk: Data) {
        guard completedResult == nil else { return }
        guard chunk.count <= maxBytes - data.count else {
            finish(.failure(Self.tooLarge(maxBytes)))
            dataTask.cancel()
            return
        }
        data.append(chunk)
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        guard completedResult == nil else { return }
        if let error {
            finish(.failure(error))
        } else if let response {
            finish(.success((data, response)))
        } else {
            finish(.failure(NSError(
                domain: "RelayClient",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "empty response"]
            )))
        }
    }

    func result() -> Result<(Data, URLResponse), Error>? {
        lock.lock()
        defer { lock.unlock() }
        return completedResult
    }

    private func finish(_ result: Result<(Data, URLResponse), Error>) {
        lock.lock()
        guard completedResult == nil else {
            lock.unlock()
            return
        }
        completedResult = result
        lock.unlock()
        semaphore.signal()
    }

    private static func tooLarge(_ maxBytes: Int) -> NSError {
        NSError(
            domain: "RelayClient",
            code: 3,
            userInfo: [NSLocalizedDescriptionKey: "relay response exceeds \(maxBytes) bytes"]
        )
    }
}

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
        let delegate = BoundedRelayResponseDelegate(
            maxBytes: Int(relayMaxResponseBytes()),
            semaphore: sem
        )
        let session = URLSession(
            configuration: urlSession.configuration,
            delegate: delegate,
            delegateQueue: nil
        )
        let task = session.dataTask(with: request)
        task.resume()
        guard sem.wait(timeout: .now() + connectTimeout + 5) == .success else {
            task.cancel()
            session.invalidateAndCancel()
            throw URLError(.timedOut)
        }
        session.finishTasksAndInvalidate()
        guard let result = delegate.result() else {
            throw malformedResponse("request completed without a result")
        }
        return try result.get()
    }

    private static func ensureOK(_ response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse else {
            throw malformedResponse("non-HTTP relay response")
        }
        guard (200..<300).contains(http.statusCode) else {
            let body = String(data: data.prefix(2_048), encoding: .utf8) ?? ""
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
