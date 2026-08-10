import XCTest
@testable import CruiseMesh

/// The exit criterion of the driver migration, iOS side: for the slice C1/C2
/// move, the request the core session forms and the request this shell puts on
/// the wire are the same bytes — the very bytes the Android suite asserts, since
/// both read `coreRelayAdapterVectors()`.
///
/// The driver request is put on the wire and compared against the core vector
/// directly (method, path, every protocol header including `Authorization`,
/// body), and against the legacy request recorded off the same mock so the two
/// engines are shown to agree rather than each agreeing with its own reading of
/// the module. Mirrors Android `RelayAdapterVectorsTest`.
///
/// # The one divergence, stated rather than absorbed
///
/// The core action carries `Accept: application/json`; the *legacy* iOS client
/// never set it (Android's legacy client does, which is why its suite finds no
/// divergence). The driver sends exactly what core forms, so the driver request
/// is byte-exact against the vector and against Android — and it is the legacy
/// iOS request that differs, by omitting `Accept`. That omission is left in
/// place on purpose: adding it to the legacy path would change the bytes a
/// `flag=legacy` pass sends, breaking this package's "byte-identical to master"
/// invariant. So `Accept` is asserted present on the driver request and absent
/// on the legacy one, and recorded here as the single iOS legacy↔core
/// difference for the receipts+authored slice.
final class RelayAdapterVectorsTests: XCTestCase {

    private static let token = "member-token"
    private static let base = "https://relay.test"
    private static let hintA = Data(repeating: 0x22, count: 8)
    private static let hintB = Data(repeating: 0x44, count: 8)

    private var previousSession: URLSession!

    override func setUp() {
        super.setUp()
        previousSession = RelayClient.urlSession
        CoreRelayFakeURLProtocol.reset()
        RelayClient.urlSession = CoreRelayFakeURLProtocol.makeSession()
        CoreRelayFakeURLProtocol.handler = { request, _ in
            let path = request.url?.path ?? ""
            let method = request.httpMethod ?? "GET"
            if path == "/envelopes", method == "POST" { return (200, [:], Data(#"{"id":7}"#.utf8)) }
            if path == "/envelopes", method == "GET" { return (200, [:], Data(#"{"envelopes":[],"next_cursor":8}"#.utf8)) }
            if path == "/envelopes/ack" { return (200, [:], Data("{}".utf8)) }
            if path == "/presence" { return (200, [:], Data(#"{"now_ms":1700000000000,"presence":[]}"#.utf8)) }
            return (200, [:], Data("{}".utf8))
        }
    }

    override func tearDown() {
        RelayClient.urlSession = previousSession
        CoreRelayFakeURLProtocol.reset()
        super.tearDown()
    }

    func testTheTableCoversTheWholeRelaySurfaceADriverExecutes() {
        XCTAssertEqual(
            coreRelayAdapterVectors().map { $0.name },
            ["post-envelope", "fetch-page", "ack-page", "presence"]
        )
    }

    func testEveryAdapterVectorIsWhatTheLegacyEngineAlreadySends() throws {
        for vector in coreRelayAdapterVectors() {
            try compare(vector)
        }
    }

    private func compare(_ vector: CoreRelayAdapterVector) throws {
        CoreRelayFakeURLProtocol.recorded = []
        let config = RelayConfig(relayUrl: Self.base, relayToken: Self.token)

        try runLegacy(vector.name, config: config)
        let legacy = try XCTUnwrap(CoreRelayFakeURLProtocol.recorded.first)

        var request = vector.request
        request.baseUrl = Self.base
        _ = RelayActionDriver.execute(passId: "p1", actionId: 1, request: request, nowMs: 1_700_000_000_000)
        let driven = try XCTUnwrap(CoreRelayFakeURLProtocol.recorded.last)

        // The driver request is the vector, byte for byte.
        XCTAssertEqual(driven.request.httpMethod, vector.request.method, "\(vector.name): method")
        XCTAssertEqual(drivenPath(driven.request), vector.request.path, "\(vector.name): path")
        XCTAssertEqual(driven.body ?? Data(), vector.request.body, "\(vector.name): body")
        for header in vector.request.headers {
            XCTAssertEqual(
                driven.request.value(forHTTPHeaderField: header.name),
                header.value,
                "\(vector.name): vector header \(header.name)"
            )
        }

        // Legacy and driver agree on the protocol-critical request.
        XCTAssertEqual(legacy.request.httpMethod, driven.request.httpMethod, "\(vector.name): method vs legacy")
        XCTAssertEqual(drivenPath(legacy.request), drivenPath(driven.request), "\(vector.name): path vs legacy")
        XCTAssertEqual(legacy.body ?? Data(), driven.body ?? Data(), "\(vector.name): body vs legacy")
        XCTAssertEqual(
            legacy.request.value(forHTTPHeaderField: "Authorization"),
            driven.request.value(forHTTPHeaderField: "Authorization"),
            "\(vector.name): Authorization vs legacy"
        )
        XCTAssertEqual(
            legacy.request.value(forHTTPHeaderField: "Content-Type"),
            driven.request.value(forHTTPHeaderField: "Content-Type"),
            "\(vector.name): Content-Type vs legacy"
        )

        // The documented divergence: legacy omits Accept, the driver (core)
        // sends it. Both halves are asserted so a future change to either side
        // is caught.
        XCTAssertEqual(driven.request.value(forHTTPHeaderField: "Accept"), "application/json", "\(vector.name): driver Accept")
        XCTAssertNil(legacy.request.value(forHTTPHeaderField: "Accept"), "\(vector.name): legacy omits Accept (known divergence)")
    }

    /// Path plus query as the vector states it, reconstructed from the recorded
    /// URL so an encoding difference would show.
    private func drivenPath(_ request: URLRequest) -> String {
        guard let url = request.url else { return "" }
        return url.path + (url.query.map { "?\($0)" } ?? "")
    }

    private func runLegacy(_ name: String, config: RelayConfig) throws {
        switch name {
        case "post-envelope":
            _ = try RelayClient.postOutboundEnvelope(config: config, envelope: vectorEnvelope())
        case "fetch-page":
            _ = try RelayClient.fetchEnvelopes(config: config, hints: [Self.hintA, Self.hintB], afterId: 8, limit: 256)
        case "ack-page":
            try RelayClient.ackEnvelopes(config: config, ids: [3, 5, 8])
        case "presence":
            _ = try RelayClient.syncPresence(config: config, announce: [Self.hintA], query: [Self.hintB])
        default:
            XCTFail("no legacy call is wired for the \(name) vector")
        }
    }

    /// The `post-envelope` vector's own field values, as a queued row. Only
    /// msg_id, hop_ttl, recipient_hint, sealed and expiry affect the encoded
    /// body; the rest match the Android vector for readability.
    private func vectorEnvelope() -> OutboundEnvelope {
        OutboundEnvelope(
            msgId: Data(repeating: 0x11, count: 16),
            recipientUserId: Data(repeating: 0x55, count: 32),
            chatId: Data(repeating: 0x66, count: 32),
            senderUserId: Data(repeating: 0x77, count: 32),
            kind: 1,
            lamport: 1,
            timestamp: 1_700_000_000_000,
            hopTtl: 4,
            expiry: 1_700_000_000_000,
            recipientHint: Self.hintA,
            sealed: Data(repeating: 0x33, count: 48)
        )
    }
}
