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

    private static func tooLarge(_ maxBytes: Int) -> RelayResponseTooLargeError {
        RelayResponseTooLargeError(maxBytes: maxBytes)
    }
}

/// The relay's answer was larger than `relayMaxResponseBytes()`, so it was
/// refused before the whole thing could be accumulated.
///
/// Its own type rather than an opaque `NSError` because it is the one
/// transport failure a caller can act on: a fetch page that blows the cap is
/// recoverable by asking the same cursor for fewer rows (see
/// `RelayClient.fetchEnvelopesWithinResponseCap`). Every other error here
/// means "try again later"; this one means "ask for less". Mirrors Android
/// `RelayResponseTooLargeException`.
struct RelayResponseTooLargeError: LocalizedError {
    let maxBytes: Int

    var errorDescription: String? {
        "relay response exceeds \(maxBytes) bytes"
    }
}

/// A fetched page plus the row limit that actually produced it. Mirrors
/// Android `RelayCappedFetch`.
struct RelayCappedFetch {
    let page: RelayFetchPage
    let limit: Int
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

struct RelayHTTPError: LocalizedError {
    let statusCode: Int
    let relayCode: String?
    let responseBody: String
    /// Raw `Retry-After` header on a 429 (CP2b); parsed/clamped by the
    /// core's `relayRetryAfterMs`, never here.
    var retryAfter: String? = nil

    var errorDescription: String? {
        let semantic = relayCode.map { " [\($0)]" } ?? ""
        return "Relay request failed (\(statusCode))\(semantic): \(responseBody)"
    }
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

    /// Fetch one page, halving `limit` and retrying the *same* cursor
    /// whenever the relay's answer is too big for this client to decode.
    /// Returns the page together with the limit that actually produced it.
    ///
    /// The stall this prevents: `limit` bounds a page's row count, not its
    /// size, and one sealed payload may be 512 KiB. A mailbox holding enough
    /// large attachment chunks can therefore produce a full-size window whose
    /// body is past `relayMaxResponseBytes()`. Without a retry the pass simply
    /// fails there; the next pass asks the same relay for the same window from
    /// the same cursor and fails identically, so the frontier never advances
    /// and nothing behind those rows is delivered until they expire.
    ///
    /// Current relayd carries a byte budget and never builds such a page, but
    /// family relays are self-hosted and older builds exist in the field, so
    /// the client cannot assume the server-side fix is there.
    ///
    /// `relayFetchShrunkLimit` returning nil means one row was already the
    /// ask: nothing smaller exists, so this is not a paging problem and the
    /// failure is raised rather than retried forever. Mirrors Android
    /// `RelayClient.fetchEnvelopesWithinResponseCap`.
    static func fetchEnvelopesWithinResponseCap(
        config: RelayConfig,
        hints: [Data],
        afterId: Int64,
        limit: Int,
        onShrink: (Int, Int) -> Void = { _, _ in }
    ) throws -> RelayCappedFetch {
        var attempt = limit
        while true {
            do {
                let page = try fetchEnvelopes(config: config, hints: hints, afterId: afterId, limit: attempt)
                return RelayCappedFetch(page: page, limit: attempt)
            } catch let error as RelayResponseTooLargeError {
                guard let smaller = relayFetchShrunkLimit(currentLimit: UInt32(clamping: attempt)) else {
                    throw error
                }
                onShrink(attempt, Int(smaller))
                attempt = Int(smaller)
            }
        }
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
        // normalizeRelayUrl returns empty for a non-HTTPS base. Every caller
        // filters those out well before here (RelayConfigStore.load and
        // resolvedContactRelay both drop them), so this is the backstop that
        // keeps a future caller from concatenating a bare path and getting an
        // opaque transport error instead of the reason. Mirrors Android
        // `RelayClient.buildUrl`.
        let normalized = normalizeRelayUrl(base)
        guard !normalized.isEmpty else {
            throw NSError(
                domain: "RelayClient",
                code: 5,
                userInfo: [NSLocalizedDescriptionKey: "Relay URL must use https"]
            )
        }
        guard let url = URL(string: normalized + path) else {
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
            let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            throw RelayHTTPError(
                statusCode: http.statusCode,
                relayCode: json?["code"] as? String,
                responseBody: body,
                retryAfter: http.value(forHTTPHeaderField: "Retry-After")
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
