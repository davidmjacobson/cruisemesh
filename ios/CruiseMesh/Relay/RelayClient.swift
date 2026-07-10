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
        let encodedHints = hints.map { base64Url($0).addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? "" }
            .joined(separator: ",")
        let url = try buildURL(config.relayUrl, path: "/envelopes?hints=\(encodedHints)&after=\(afterId)&limit=\(limit)")
        var request = URLRequest(url: url, timeoutInterval: connectTimeout)
        request.httpMethod = "GET"
        applyAuth(&request, config: config)
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
        let items = json["envelopes"] as? [[String: Any]] ?? []
        let nextCursor = (json["next_cursor"] as? NSNumber)?.int64Value ?? afterId
        let envelopes: [RelayFetchedEnvelope] = try items.map { item in
            RelayFetchedEnvelope(
                id: (item["id"] as? NSNumber)?.int64Value ?? 0,
                msgId: try base64UrlDecode(item["msg_id"] as? String ?? ""),
                hopTtl: UInt8((item["hop_ttl"] as? NSNumber)?.intValue ?? 0),
                recipientHint: try base64UrlDecode(item["recipient_hint"] as? String ?? ""),
                sealed: try base64UrlDecode(item["sealed"] as? String ?? ""),
                expiryMs: (item["expiry_ms"] as? NSNumber)?.int64Value ?? 0
            )
        }
        return RelayFetchPage(envelopes: envelopes, nextCursor: nextCursor)
    }

    static func ackEnvelopes(config: RelayConfig, ids: [Int64]) throws {
        guard !ids.isEmpty else { return }
        let url = try buildURL(config.relayUrl, path: "/envelopes/ack")
        var request = URLRequest(url: url, timeoutInterval: connectTimeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        applyAuth(&request, config: config)
        let body: [String: Any] = ["ids": ids]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
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
        let body: [String: Any] = [
            "msg_id": base64Url(msgId),
            "hop_ttl": Int(hopTtl),
            "recipient_hint": base64Url(recipientHint),
            "sealed": base64Url(sealed),
            "expiry_ms": expiryMs,
        ]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try syncRequest(request)
        try ensureOK(response, data: data)
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
        return (json["id"] as? NSNumber)?.int64Value ?? 0
    }

    private static func applyAuth(_ request: inout URLRequest, config: RelayConfig) {
        request.setValue("Bearer \(config.relayToken)", forHTTPHeaderField: "Authorization")
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        request.setValue("1", forHTTPHeaderField: "Bypass-Tunnel-Reminder")
    }

    private static func buildURL(_ base: String, path: String) throws -> URL {
        let trimmed = base.hasSuffix("/") ? String(base.dropLast()) : base
        guard let url = URL(string: trimmed + path) else {
            throw NSError(domain: "RelayClient", code: 1, userInfo: [NSLocalizedDescriptionKey: "bad URL"])
        }
        return url
    }

    private static func syncRequest(_ request: URLRequest) throws -> (Data, URLResponse) {
        let sem = DispatchSemaphore(value: 0)
        var result: Result<(Data, URLResponse), Error>!
        urlSession.dataTask(with: request) { data, response, error in
            if let error {
                result = .failure(error)
            } else if let data, let response {
                result = .success((data, response))
            } else {
                result = .failure(NSError(domain: "RelayClient", code: 2, userInfo: [NSLocalizedDescriptionKey: "empty response"]))
            }
            sem.signal()
        }.resume()
        _ = sem.wait(timeout: .now() + connectTimeout + 5)
        return try result.get()
    }

    private static func ensureOK(_ response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse else { return }
        guard (200..<300).contains(http.statusCode) else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw NSError(
                domain: "RelayClient",
                code: http.statusCode,
                userInfo: [NSLocalizedDescriptionKey: "HTTP \(http.statusCode): \(body)"]
            )
        }
    }

    private static func base64Url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private static func base64UrlDecode(_ s: String) throws -> Data {
        var str = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        let pad = (4 - str.count % 4) % 4
        str += String(repeating: "=", count: pad)
        guard let data = Data(base64Encoded: str) else {
            throw NSError(domain: "RelayClient", code: 3, userInfo: [NSLocalizedDescriptionKey: "bad base64"])
        }
        return data
    }
}
