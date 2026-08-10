import Foundation
import os.log

/// One typed relay action, executed. Nothing else. The iOS counterpart of
/// Android's `CoreRelayDriver`, seam for seam.
///
/// The core relay session decides everything about a request — method, path,
/// every header including `Authorization`, the body bytes, how much of a
/// response may be read, and which response headers it wants back. This puts
/// exactly that on the wire and hands back a bounded, typed result. It does not
/// retry, does not shrink a page, does not decide whether a row may be acked,
/// does not advance a cursor, does not interpret a status code, and does not
/// know what a pass is. Every one of those is a decision that needs state this
/// deliberately cannot see, and a driver that inferred even one of them would
/// be a second place the protocol is decided.
///
/// # What it does own
///
/// **The socket, and the transport it rides.** `RelayClient` is shared with the
/// legacy engine on purpose — the connect timeout, the inactivity watchdog and
/// the transport headers come from one place, so the two engines cannot drift
/// apart below the protocol layer. iOS pins no interface for relay sync (it
/// runs on `URLSession.shared`), which is the same rule the legacy iOS path
/// follows, so there is no `Network` parameter here as there is on Android.
///
/// **The response cap.** Core declares `CoreRelayHttpRequest.maxResponseBytes`;
/// this enforces it with the same bounded read the legacy fetch walk uses, and
/// reports a page too big to take as `CoreRelayTransportError.bodyTooLarge`,
/// which core answers by asking for fewer rows from the same cursor — so it
/// must not be reported for anything that is not a page too big to take.
///
/// **Which failure it was.** A socket that never connected, a TLS handshake
/// that failed, a body that stopped arriving, a cancellation: core folds these
/// into health and silence evidence, so they arrive as distinct typed values
/// rather than as one "it failed".
///
/// # Status before size
///
/// A non-2xx body is read as a short preview and never as an oversize failure:
/// a captive-portal notice, a proxy banner or a gateway error page can be any
/// size, and calling one an oversized *page* both sends a fetch down the shrink
/// ladder and throws away the `Retry-After` header on a 429 — the one header
/// `RATE-01` is measured from. `RelayClient.performCoreTransport` returns a
/// non-2xx normally with its status and preview, which keeps that rule here.
enum RelayActionDriver {

    /// Run one action.
    ///
    /// Never throws: every failure is a typed result, because a driver that
    /// threw would make the session's "one outstanding action" invariant the
    /// caller's problem to unwind.
    ///
    /// - Parameter nowMs: the wall clock to stamp the result with, supplied
    ///   rather than read so a test can run a pass on a fake clock and so the
    ///   session is never handed a time this chose.
    /// - Parameter isCancelled: consulted before the request is issued and
    ///   again after it completes; the process being backgrounded mid-pass is a
    ///   cancellation, not an outage, and telling core otherwise would let a
    ///   backgrounded app accumulate silence evidence against healthy endpoints.
    static func execute(
        passId: String,
        actionId: UInt64,
        request: CoreRelayHttpRequest,
        nowMs: Int64,
        isCancelled: () -> Bool = { false }
    ) -> CoreRelayHttpResult {
        if isCancelled() {
            return failure(passId, actionId, .cancelled, nowMs)
        }
        do {
            let urlRequest = try RelayClient.coreTransportRequest(from: request)
            let outcome = try RelayClient.performCoreTransport(
                urlRequest,
                maxBytes: Int(request.maxResponseBytes)
            )
            if isCancelled() {
                return failure(passId, actionId, .cancelled, nowMs)
            }
            return CoreRelayHttpResult(
                passId: passId,
                actionId: actionId,
                status: UInt16(clamping: outcome.status),
                headers: selectedHeaders(request, outcome.response),
                body: outcome.body,
                error: nil,
                completedAtMs: nowMs
            )
        } catch {
            return failure(passId, actionId, relayClassifyTransportError(error), nowMs)
        }
    }

    /// Only the headers core asked for. Everything else is dropped here rather
    /// than passed along and ignored: a response header core never requested
    /// cannot reach a store, an event or a summary if it never crosses the
    /// boundary in the first place.
    private static func selectedHeaders(
        _ request: CoreRelayHttpRequest,
        _ response: HTTPURLResponse
    ) -> [CoreRelayHeader] {
        request.responseHeadersWanted.compactMap { name in
            response.value(forHTTPHeaderField: name).map { CoreRelayHeader(name: name, value: $0) }
        }
    }

    private static func failure(
        _ passId: String,
        _ actionId: UInt64,
        _ error: CoreRelayTransportError,
        _ nowMs: Int64
    ) -> CoreRelayHttpResult {
        CoreRelayHttpResult(
            passId: passId,
            actionId: actionId,
            // Zero, not a synthesized status: core distinguishes "the relay
            // answered badly" from "there was no answer", and the second must
            // not be able to masquerade as the first.
            status: 0,
            headers: [],
            body: Data(),
            error: error,
            completedAtMs: nowMs
        )
    }
}

/// Which typed failure a thrown error was.
///
/// A free function rather than a member of `RelayActionDriver`, and that is not
/// tidiness. The migration canary needs this mapping to describe a failure the
/// *legacy* engine saw, and reaching it through the driver would put the object
/// that opens sockets inside the canary's reach. The classification is a pure
/// function of an error and belongs to neither engine, so it lives beside both.
///
/// `RelayPageTooBigError` — both `RelayResponseTooLargeError` (a body past the
/// cap) and `RelayResponseStalledError` (a body that stopped after the head) —
/// maps to `bodyTooLarge`, because on a link that will not carry a full page
/// both are the same permanent stall from the same cursor and asking for fewer
/// rows is the same fix. A timeout while *connecting* or waiting for the status
/// line says nothing about page size and stays a plain timeout.
func relayClassifyTransportError(_ error: Error) -> CoreRelayTransportError {
    if error is RelayPageTooBigError { return .bodyTooLarge }
    guard let urlError = error as? URLError else { return .other }
    switch urlError.code {
    case .cancelled:
        return .cancelled
    case .timedOut:
        return .timeout
    case .secureConnectionFailed,
         .serverCertificateUntrusted,
         .serverCertificateHasBadDate,
         .serverCertificateHasUnknownRoot,
         .serverCertificateNotYetValid,
         .clientCertificateRejected,
         .clientCertificateRequired:
        return .tls
    case .cannotConnectToHost,
         .cannotFindHost,
         .networkConnectionLost,
         .notConnectedToInternet,
         .dnsLookupFailed,
         .resourceUnavailable:
        return .connectionFailed
    default:
        return .other
    }
}

/// Where one typed relay action is actually performed.
///
/// A one-method seam, and the whole reason the pass runner needs no
/// `URLSession` of its own: production hands it a driver that opens the socket,
/// and a test hands it a scripted relay. Neither the runner nor the core
/// session can tell the difference, which is what makes a full core pass
/// something a unit test can drive end to end. Mirrors Android
/// `RelayActionExecutor`.
protocol RelayActionExecutor {
    func execute(
        passId: String,
        actionId: UInt64,
        request: CoreRelayHttpRequest,
        nowMs: Int64
    ) -> CoreRelayHttpResult
}

/// The production executor: paces against the family request budget, then
/// hands the action to `RelayActionDriver` with the pass's cancellation check.
///
/// The pace is the same reservation the legacy engine makes, because the budget
/// belongs to the family's relay token, not to whichever engine happens to be
/// spending it.
struct LiveRelayActionExecutor: RelayActionExecutor {
    /// Consulted before and after each request; the process going away mid-pass
    /// is a cancellation, not an outage.
    let isCancelled: () -> Bool
    /// Reserves this device's slot in the family request pacer, sleeping the
    /// calling thread for the returned wait. Runs on the detached relay task,
    /// never the main thread.
    let pace: () -> Void

    init(isCancelled: @escaping () -> Bool = { false }, pace: @escaping () -> Void = {}) {
        self.isCancelled = isCancelled
        self.pace = pace
    }

    func execute(
        passId: String,
        actionId: UInt64,
        request: CoreRelayHttpRequest,
        nowMs: Int64
    ) -> CoreRelayHttpResult {
        pace()
        return RelayActionDriver.execute(
            passId: passId,
            actionId: actionId,
            request: request,
            nowMs: nowMs,
            isCancelled: isCancelled
        )
    }
}

/// Drives a `CoreRelayPass` from its first action to its summary. The iOS
/// counterpart of Android's `CoreRelayPassRunner`.
///
/// This is the entire shell-side orchestration of a core relay pass, and its
/// shortness is the point: hand core the plan, ask for an action, do exactly
/// that action, hand back exactly what happened, repeat. There is no branch
/// here on a status code, no retry, no cursor, no marker, no health decision —
/// every one of those has already been made by the time an action arrives.
///
/// The one judgement it makes is when to stop asking, and it makes it the
/// defensive way. `LIVE-01` says a pass terminates inside its declared budgets,
/// and the core session enforces that; this loop is the backstop for a session
/// that somehow does not, because a driver that spins forever is the failure a
/// person experiences as a dead battery rather than as a bug report.
final class RelaySyncDriver {

    private let store: MessageStore
    private let executor: RelayActionExecutor
    private let clock: () -> Int64
    private let isCancelled: () -> Bool

    init(
        store: MessageStore,
        executor: RelayActionExecutor,
        clock: @escaping () -> Int64,
        isCancelled: @escaping () -> Bool = { false }
    ) {
        self.store = store
        self.executor = executor
        self.clock = clock
        self.isCancelled = isCancelled
    }

    /// Run one pass and return what it did.
    ///
    /// - Parameter passId: a short opaque label a transcript can be read by.
    ///   Core derives the id it actually carries from this, so two passes can
    ///   never share one however this is called.
    func run(plan: CoreRelayPassPlan, passId: String) -> CoreRelayPassSummary {
        let pass = CoreRelayPass(store: store, plan: plan, passId: passId)
        // Every request the budget permits, plus the acks pages earn, plus
        // room: high enough that no lawful pass reaches it, low enough that an
        // unlawful one is stopped in seconds rather than in a battery.
        let guardLimit = Int64(plan.budgets.maxRequests) * 2 + 64
        var issued: Int64 = 0
        var action = pass.start(nowMs: clock())

        while true {
            switch action.kind {
            case .finished(let summary):
                return summary

            // A sleep means the pass refused to spend inside a quiet window and
            // has already finished; the wait itself belongs to whatever
            // schedules the next pass, not to this loop.
            case .sleep:
                return pass.summary() ?? pass.cancel(nowMs: clock())

            // Unreachable after start(), and treated as an ended pass rather
            // than as a reason to call start() again: a second start would
            // re-run stage one against a store the first call already pruned.
            case .notStarted:
                return pass.cancel(nowMs: clock())

            case .http(let request):
                if isCancelled() {
                    return pass.cancel(nowMs: clock())
                }
                if issued >= guardLimit {
                    log.error("Core relay pass issued \(issued, privacy: .public) actions without finishing; cancelling")
                    try? store.noteInvariantViolation(
                        invariantId: "LIVE-01",
                        outcome: "pass_exceeded_driver_guard",
                        nowMs: clock()
                    )
                    return pass.cancel(nowMs: clock())
                }
                issued += 1
                let result = executor.execute(
                    passId: action.passId,
                    actionId: action.actionId,
                    request: request,
                    nowMs: clock()
                )
                action = pass.resumeHttp(result: result)
            }
        }
    }

    private let log = Logger(subsystem: "com.cruisemesh", category: "RelayClient")
}
