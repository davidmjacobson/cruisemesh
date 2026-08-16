import XCTest
@testable import CruiseMesh

/// The incident corpus, executed through this platform's relay adapter.
///
/// Until now the fixtures under `core/tests/fixtures/` were executed only in
/// Rust, and the only thing the two shells' adapters shared was a table of four
/// request shapes. A request shape says nothing about whether a whole incident
/// — several passes, one store, a relay that stops answering part-way — ends in
/// the same state once a real driver and a real HTTP client are in the loop.
///
/// Each case here takes one fixture scenario from core, seeds a real
/// `MessageStore` from core, drives every pass through the *production*
/// `RelaySyncDriver` and `RelayActionDriver` against a `URLProtocol` answering
/// core's script, and compares the normalised transcript against
/// `coreRelayFixtureExpectedTranscript` — the same scenario run in Rust with
/// nothing but the HTTP replaced.
///
/// The Android twin is `RelayAdapterFixtureTranscriptTest.kt`, comparing
/// against the same string. That is the paired-platform claim the driver
/// migration actually needs: not that each shell is self-consistent, but that
/// the same incident produces the same outcome on both.
///
/// # What a failure here means
///
/// The transcript carries, per pass, every request as *the server received it*,
/// how the driver reported each answer, the pass summary, and then the store
/// state, the emitted protocol-event codes and any invariant the session
/// reported violated. So a red here is one of: a driver that mangled a query
/// string, dropped a body, altered a header, swallowed or invented a status,
/// mislabelled a transport failure as an answer, or a runner that issued the
/// wrong number of actions. Every one of those is invisible to a per-request
/// vector test and is exactly what a migration produces.
///
/// # Scope
///
/// The fixtures wired today are `carry-storm`, `contact-silence-no-proof`,
/// `group-fanout-complete` and `group-fanout-partial`. Adding another is one
/// arm in core's scenario table; this file iterates `coreRelayFixtureNames()`
/// and needs no change — the two group-lane fixtures landed without a line
/// changing here.
final class RelayAdapterFixtureTranscriptTests: XCTestCase {

    /// Two distinct hosts because a contact's mailbox lives on its own relay,
    /// and `SILENCE-01` is about telling "the contact is quiet" apart from
    /// "this phone is offline" — a distinction that does not exist if both
    /// endpoints are the same host. Neither is ever resolved: `URLProtocol`
    /// answers before a socket is opened.
    private static let ownBase = "https://own-relay.invalid"
    private static let contactBase = "https://contact-relay.invalid"
    private static let ownToken = "member-token-own"
    private static let contactToken = "member-token-contact"

    private var previousSession: URLSession!

    override func setUp() {
        super.setUp()
        previousSession = RelayClient.urlSession
        CoreRelayFakeURLProtocol.reset()
        RelayClient.urlSession = CoreRelayFakeURLProtocol.makeSession()
    }

    override func tearDown() {
        RelayClient.urlSession = previousSession
        CoreRelayFakeURLProtocol.reset()
        super.tearDown()
    }

    func testEveryWiredFixtureDrivesThroughTheRealAdapterToTheTranscriptCoreExpects() throws {
        let names = coreRelayFixtureNames()
        XCTAssertFalse(names.isEmpty, "the corpus must wire at least one fixture")
        for name in names {
            let (transcript, _) = try drive(name)
            XCTAssertEqual(
                transcript,
                coreRelayFixtureExpectedTranscript(name: name),
                "\(name): the transcript this adapter produced differs from the reference run"
            )
        }
    }

    func testNoFixtureScenarioReportsOneOfItsDeclaredInvariantsViolated() throws {
        // The same claim `relay_pass_replay.rs` makes in Rust, made here about
        // a store this platform's own driver filled. It is redundant with the
        // comparison above by construction and stated anyway: this is the
        // sentence a person reads when it goes red, and "the strings differ" is
        // not that sentence.
        for name in coreRelayFixtureNames() {
            let (_, store) = try drive(name)
            let violated = coreRelayFixtureViolatedInvariants(store: store)
            for declared in coreRelayFixtureScenario(name: name).declaredInvariants {
                XCTAssertFalse(
                    violated.contains(declared),
                    "\(name): the session reported \(declared) violated"
                )
            }
        }
    }

    // MARK: - the harness

    /// Run one scenario end to end and return its transcript and the store it
    /// left behind.
    private func drive(_ name: String) throws -> (String, MessageStore) {
        let scenario = coreRelayFixtureScenario(name: name)
        let store = try MessageStore.open(path: ":memory:")
        coreRelayFixtureSeedStore(store: store, name: name)

        let relay = ScriptedRelay()
        relay.install()
        defer { relay.uninstall() }

        let transcript = CoreRelayFixtureTranscript(fixture: name)

        for (index, spec) in scenario.passes.enumerated() {
            let passIndex = UInt32(index)
            let executor = ScriptedExecutor(
                name: name,
                passIndex: passIndex,
                ownBase: Self.ownBase,
                relay: relay,
                transcript: transcript
            )
            let summary = RelaySyncDriver(
                store: store,
                executor: executor,
                clock: { spec.nowMs }
            ).run(
                plan: coreRelayFixturePlan(
                    name: name,
                    passIndex: passIndex,
                    ownUrl: Self.ownBase,
                    ownToken: Self.ownToken,
                    contactUrl: Self.contactBase,
                    contactToken: Self.contactToken
                ),
                passId: spec.label
            )
            transcript.recordSummary(passIndex: passIndex, spec: spec, summary: summary)
        }

        return (
            transcript.finish(store: store, ownUrl: Self.ownBase, ownToken: Self.ownToken),
            store
        )
    }

    /// Wraps the production driver: script the relay for the action about to be
    /// issued, run it through `RelayActionDriver`, then record what the server
    /// saw and what the driver reported.
    ///
    /// The recording is the point. The transcript's request lines come from the
    /// bytes that reached the server, not from the `CoreRelayHttpRequest` core
    /// formed, so a driver that alters one between the two is what this catches.
    private struct ScriptedExecutor: RelayActionExecutor {
        let name: String
        let passIndex: UInt32
        let ownBase: String
        let relay: ScriptedRelay
        let transcript: CoreRelayFixtureTranscript

        func execute(
            passId: String,
            actionId: UInt64,
            request: CoreRelayHttpRequest,
            nowMs: Int64
        ) -> CoreRelayHttpResult {
            // Which configured endpoint this is for: an addressing decision,
            // not a protocol one. Core chose the base URL; the harness only has
            // to recognise which of the two it configured that is.
            let endpoint: CoreRelayFixtureEndpoint =
                request.baseUrl == ownBase ? .own : .contact
            relay.answerNext(
                with: coreRelayFixtureReply(
                    name: name,
                    passIndex: passIndex,
                    operation: request.operation,
                    endpoint: endpoint
                )
            )

            let result = RelayActionDriver.execute(
                passId: passId,
                actionId: actionId,
                request: request,
                nowMs: nowMs
            )

            transcript.recordRequest(
                passIndex: passIndex,
                request: request,
                endpoint: endpoint,
                observed: relay.observation(of: request)
            )
            transcript.recordResult(passIndex: passIndex, result: result)
            return result
        }
    }

    /// A relay that answers exactly what core's script says and records what it
    /// was asked. Everything is set immediately before the driver call on the
    /// same thread, so the plain properties need no synchronisation.
    private final class ScriptedRelay {

        private var next: CoreRelayFixtureReply?
        private var observed: CoreRelayFixtureObservedRequest?

        func install() {
            CoreRelayFakeURLProtocol.reset()
            CoreRelayFakeURLProtocol.handler = { [weak self] request, body in
                guard let self, let reply = self.next else { return (200, [:], Data("{}".utf8)) }
                if self.observed == nil {
                    self.observed = CoreRelayFixtureObservedRequest(
                        method: request.httpMethod ?? "",
                        path: Self.pathWithQuery(request),
                        bodyLen: UInt32(body?.count ?? 0),
                        authorization: request.value(forHTTPHeaderField: "Authorization")
                    )
                }
                // nil is a relay that could not be reached at all rather than
                // one that answered badly, which is what `SILENCE-01` turns on.
                if reply.transportFailure { return nil }
                var headers: [String: String] = [:]
                for header in reply.headers { headers[header.name] = header.value }
                return (Int(reply.status), headers, reply.body)
            }
        }

        func uninstall() {
            CoreRelayFakeURLProtocol.reset()
        }

        func answerNext(with reply: CoreRelayFixtureReply) {
            next = reply
            observed = nil
        }

        /// What the server saw, or core's own form of the request if the fake
        /// transport was never reached. There is nothing else to compare in
        /// that case, and dropping the line instead would make the two
        /// platforms' transcripts differ over which of them managed to observe
        /// a request it was always going to fail.
        func observation(of request: CoreRelayHttpRequest) -> CoreRelayFixtureObservedRequest {
            observed ?? coreRelayFixtureIdealObservation(request: request)
        }

        private static func pathWithQuery(_ request: URLRequest) -> String {
            guard let url = request.url else { return "" }
            guard let query = url.query else { return url.path }
            return "\(url.path)?\(query)"
        }
    }
}
