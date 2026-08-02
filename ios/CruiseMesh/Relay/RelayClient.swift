import Foundation

/// Accumulates one bounded relay response and decides what its failures mean.
///
/// Internal rather than file-private only so the tests can drive its callbacks
/// by hand. That matters here: the cases worth pinning are "the body stopped
/// after the head arrived" and "nothing arrived at all", and staging those
/// through a mock `URLProtocol` means racing the URL loading system's own
/// callback scheduling. Called directly, the same decisions are exercised with
/// no timers and no ordering to lose.
final class BoundedRelayResponseDelegate: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let maxBytes: Int
    private let errorPreviewBytes: Int
    private let semaphore: DispatchSemaphore
    private let lock = NSLock()
    private var data = Data()
    private var response: URLResponse?
    private var completedResult: Result<(Data, URLResponse), Error>?
    /// Bumped on every piece of progress the transfer makes. `RelayClient`
    /// polls it to tell "this download is slow" apart from "this download
    /// stopped", so a big page on a weak ship Wi-Fi is waited out rather than
    /// killed. Guarded by `lock`: it is written on the delegate queue and read
    /// from the calling thread.
    private var activity: UInt64 = 0
    /// Whether the response head arrived. A transfer that stops *after* the
    /// head is a page that will not move over this link; one that stops before
    /// it is a relay that cannot be reached at all. The two want different
    /// recoveries.
    private var headReceived = false
    /// How much of the body this response is willing to keep: the whole cap
    /// for a success, a short preview for an HTTP error (see `didReceive
    /// response`).
    private var acceptBytes: Int
    private var isErrorStatus = false

    init(maxBytes: Int, errorPreviewBytes: Int, semaphore: DispatchSemaphore) {
        self.maxBytes = maxBytes
        self.errorPreviewBytes = errorPreviewBytes
        self.acceptBytes = maxBytes
        self.semaphore = semaphore
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        noteProgress(head: true)
        self.response = response
        // Status before size. A captive portal notice, a proxy banner or a
        // gateway error page can be any size at all, and calling one an
        // oversized *page* sends the fetch down the shrink ladder -- eight
        // more round trips that were never going to succeed -- and throws away
        // a 429's `Retry-After` on the way. A non-2xx body is only ever read
        // to name the failure, so a short preview of it is all this keeps.
        let status = (response as? HTTPURLResponse)?.statusCode
        isErrorStatus = status.map { !(200..<300).contains($0) } ?? false
        if isErrorStatus {
            acceptBytes = errorPreviewBytes
            completionHandler(.allow)
            return
        }
        acceptBytes = maxBytes
        if response.expectedContentLength > Int64(maxBytes) {
            finish(.failure(Self.tooLarge(maxBytes)))
            completionHandler(.cancel)
            return
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive chunk: Data) {
        guard result() == nil else { return }
        noteProgress(head: false)
        guard chunk.count <= acceptBytes - data.count else {
            guard isErrorStatus, let response else {
                finish(.failure(Self.tooLarge(maxBytes)))
                dataTask.cancel()
                return
            }
            // Enough of the error page to quote in the failure; the rest is
            // noise nobody reads, so stop here instead of buffering it.
            data.append(chunk.prefix(acceptBytes - data.count))
            finish(.success((data, response)))
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
        guard result() == nil else { return }
        if let error {
            finish(.failure(classify(error)))
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

    /// A monotonically increasing marker of transfer progress. Two reads that
    /// return the same value mean nothing moved in between.
    func activityMarker() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return activity
    }

    /// Describes the stall if -- and only if -- the response head is already
    /// in. Before that there is no page to shrink, just an unreachable relay.
    func stalledMidResponse() -> RelayResponseStalledError? {
        lock.lock()
        defer { lock.unlock() }
        guard headReceived else { return nil }
        return RelayResponseStalledError(bytesReceived: data.count)
    }

    /// URLSession applies `timeoutIntervalForRequest` as its own inactivity
    /// timeout, so it can raise `.timedOut` mid-body before this client's
    /// watchdog does. Same situation, same recovery.
    private func classify(_ error: Error) -> Error {
        guard (error as? URLError)?.code == .timedOut, let stalled = stalledMidResponse() else {
            return error
        }
        return stalled
    }

    private func noteProgress(head: Bool) {
        lock.lock()
        activity &+= 1
        if head {
            headReceived = true
        }
        lock.unlock()
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

/// A fetch failure that asking for fewer rows can fix.
///
/// These are the transport failures a caller can act on, as opposed to the
/// ones that only mean "try again later": a page can be too big to decode, or
/// too big to move over the link it was asked for, and both are answered the
/// same way -- same cursor, smaller window (see
/// `RelayClient.fetchEnvelopesWithinResponseCap`). Mirrors Android
/// `RelayPageTooBigException`.
protocol RelayPageTooBigError: Error {}

/// The relay's answer was larger than `relayMaxResponseBytes()`, so it was
/// refused before the whole thing could be accumulated. Mirrors Android
/// `RelayResponseTooLargeException`.
struct RelayResponseTooLargeError: RelayPageTooBigError, LocalizedError {
    let maxBytes: Int

    var errorDescription: String? {
        "relay response exceeds \(maxBytes) bytes"
    }
}

/// The relay answered, and then the body stopped arriving.
///
/// On a ship's Wi-Fi a full page can be megabytes, and a link that cannot
/// carry it in the time allowed will fail the same way on the next pass, from
/// the same cursor -- the same permanent stall an undecodable page causes. So
/// it gets the same treatment: ask for fewer rows and let the mail through
/// slowly rather than not at all. Distinguished from a plain connect timeout,
/// which says nothing about page size. Mirrors Android
/// `RelayResponseStalledException`.
struct RelayResponseStalledError: RelayPageTooBigError, LocalizedError {
    let bytesReceived: Int

    var errorDescription: String? {
        "relay response stalled after \(bytesReceived) bytes"
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
    /// How long the transfer may make *no progress at all* before it is given
    /// up on. Deliberately not a wall-clock budget for the whole request: a
    /// full fetch page can be megabytes, and a ship's Wi-Fi that needs a
    /// minute to carry it is slow, not broken. Killing it on the clock would
    /// fail the same window from the same cursor on every pass forever.
    /// Matches Android, whose `readTimeout` is likewise per-read.
    private static let inactivityTimeout: TimeInterval = 15
    /// How much of a non-2xx body is kept -- enough to quote the relay's
    /// reason in the error, not enough for an error page to cost memory.
    private static let errorBodyPreviewBytes = 2_048
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
    /// whenever the relay's answer is too big for this client to take --
    /// either too big to decode, or too big to finish moving over this link.
    /// Returns the page together with the limit that actually produced it.
    ///
    /// The stall this prevents: `limit` bounds a page's row count, not its
    /// size, and one sealed payload may be 512 KiB. A mailbox holding enough
    /// large attachment chunks can therefore produce a full-size window whose
    /// body is past `relayMaxResponseBytes()`, or simply past what a ship's
    /// Wi-Fi will carry before the transfer is written off. Without a retry
    /// the pass simply fails there; the next pass asks the same relay for the
    /// same window from the same cursor and fails identically, so the frontier
    /// never advances and nothing behind those rows is delivered until they
    /// expire.
    ///
    /// Current relayd carries a byte budget and never builds an undecodable
    /// page, but family relays are self-hosted and older builds exist in the
    /// field, so the client cannot assume the server-side fix is there -- and
    /// no server-side budget can make a slow link fast.
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
            } catch let error as RelayPageTooBigError {
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
            errorPreviewBytes: errorBodyPreviewBytes,
            semaphore: sem
        )
        let session = URLSession(
            configuration: urlSession.configuration,
            delegate: delegate,
            delegateQueue: nil
        )
        let task = session.dataTask(with: request)
        task.resume()
        // The wait is bounded by inactivity, not by total time. Each slice
        // that expires without the task finishing asks the delegate whether
        // anything arrived meanwhile; as long as bytes keep coming the
        // download is left alone, however long it takes. Connecting stays
        // bounded as before -- `URLRequest.timeoutInterval` is `connectTimeout`
        // and URLSession enforces it -- and a transfer that truly goes quiet
        // is still cut off here.
        var lastActivity = delegate.activityMarker()
        while sem.wait(timeout: .now() + inactivityTimeout) != .success {
            let marker = delegate.activityMarker()
            guard marker == lastActivity else {
                lastActivity = marker
                continue
            }
            let stalled = delegate.stalledMidResponse()
            task.cancel()
            session.invalidateAndCancel()
            // A body that stopped part-way is a page this link will not carry:
            // reported as its own type so the fetch walk shrinks the window
            // instead of retrying the identical one forever. Nothing arriving
            // at all says nothing about page size, and stays a plain timeout.
            if let stalled {
                throw stalled
            }
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
            // Already truncated to a preview by the delegate; bounded again
            // here so this stays correct for any other caller.
            let body = String(data: data.prefix(errorBodyPreviewBytes), encoding: .utf8) ?? ""
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
