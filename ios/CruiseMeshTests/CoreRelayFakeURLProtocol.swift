import Foundation
@testable import CruiseMesh

/// A `URLProtocol` that answers relay requests from a dispatch closure and
/// records what it was asked, so a whole core relay pass can be driven over the
/// same bounded transport the app uses without a real socket.
///
/// The queue-based `RelayMockURLProtocol` in `RelayClientTests` answers one
/// canned response per call; a pass interleaves posts, fetches and acks whose
/// answers depend on the path and method, so this one dispatches instead. A
/// handler returning nil is a relay that could not be reached at all, reported
/// as `URLError(.cannotConnectToHost)` — the shape a closed port produces.
final class CoreRelayFakeURLProtocol: URLProtocol {

    struct Recorded {
        let request: URLRequest
        let body: Data?
    }

    /// Returns (status, headers, body) for a request, or nil to fail the
    /// transport. Set before the request is issued.
    static var handler: ((URLRequest, Data?) -> (Int, [String: String], Data)?)?
    static var recorded: [Recorded] = []

    static func reset() {
        handler = nil
        recorded = []
    }

    /// A `URLSession` wired to this protocol, for `RelayClient.urlSession`.
    static func makeSession() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [CoreRelayFakeURLProtocol.self]
        return URLSession(configuration: config)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let body = Self.readBody(from: request)
        Self.recorded.append(Recorded(request: request, body: body))
        guard let handler = Self.handler, let answer = handler(request, body) else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost))
            return
        }
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: answer.0,
            httpVersion: "HTTP/1.1",
            headerFields: answer.1
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        if !answer.2.isEmpty {
            client?.urlProtocol(self, didLoad: answer.2)
        }
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    private static func readBody(from request: URLRequest) -> Data? {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return nil }
        stream.open()
        defer { stream.close() }
        let bufferSize = 4_096
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: bufferSize)
        defer { buffer.deallocate() }
        var body = Data()
        while true {
            let read = stream.read(buffer, maxLength: bufferSize)
            if read < 0 { return nil }
            if read == 0 { return body }
            body.append(buffer, count: read)
        }
    }
}

/// A relay that answers, counts, and can be told to say no — the iOS twin of
/// the Android `CoreRelayPassRunnerTest.FakeRelay`, over `URLProtocol` rather
/// than a socket. Install it with `install()` and remove it with `remove()`.
final class CoreRelayFakeRelay {

    private(set) var posts = 0
    private(set) var acks = 0
    private(set) var ackBody = ""
    private(set) var fetchPaths: [String] = []

    /// The page a fetch answers with; an empty mailbox by default.
    var fetchBody = #"{"envelopes":[],"next_cursor":0}"#
    /// When set, overrides `fetchBody` and every field of the fetch answer.
    var fetchResponse: (() -> (Int, [String: String], Data))?
    /// The post answer; a fresh id by default.
    var postResponse: () -> (Int, [String: String], Data) = { (200, [:], Data(#"{"id":1}"#.utf8)) }

    private var previousSession: URLSession?

    func install() {
        previousSession = RelayClient.urlSession
        CoreRelayFakeURLProtocol.reset()
        RelayClient.urlSession = CoreRelayFakeURLProtocol.makeSession()
        CoreRelayFakeURLProtocol.handler = { [weak self] request, body in
            guard let self else { return (200, [:], Data("{}".utf8)) }
            let path = request.url.map { $0.path + ($0.query.map { "?\($0)" } ?? "") } ?? ""
            let method = request.httpMethod ?? "GET"
            if path == "/envelopes", method == "POST" {
                self.posts += 1
                return self.postResponse()
            }
            if path == "/envelopes/ack", method == "POST" {
                self.acks += 1
                self.ackBody = body.flatMap { String(data: $0, encoding: .utf8) } ?? ""
                return (200, [:], Data("{}".utf8))
            }
            if path.hasPrefix("/envelopes?"), method == "GET" {
                self.fetchPaths.append(path)
                if let fetchResponse = self.fetchResponse { return fetchResponse() }
                return (200, [:], Data(self.fetchBody.utf8))
            }
            if path == "/presence", method == "POST" {
                return (200, [:], Data(#"{"now_ms":1700000000000,"presence":[]}"#.utf8))
            }
            return (200, [:], Data("{}".utf8))
        }
    }

    func remove() {
        if let previousSession { RelayClient.urlSession = previousSession }
        CoreRelayFakeURLProtocol.reset()
    }
}

/// Base64url without padding, matching the relay wire encoding.
func relayBase64Url(_ data: Data) -> String {
    data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}
