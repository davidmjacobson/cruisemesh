import Foundation
import os.log

/// The one thing the canary is allowed to write.
///
/// A struct wrapping a single closure rather than the message store, and that
/// is the difference between "the shadow does not write" as an intention and as
/// a property. `MessageStore` is the single object carrying every operational
/// write in the app as a public method: an adapter holding one has every one of
/// them within reach of a single line, and only a reviewer's attention stands
/// between the two engines and a second writer. An adapter holding *this*
/// cannot express one — the only call it can reach touches the bounded
/// diagnostics ring and takes a report that cannot be turned back into a row.
/// Mirrors Android `RelayShadowReportSink`.
struct RelayShadowReportSink {
    let note: (CoreRelayShadowReport, Int64) -> Void
}

/// One contact, as the canary needs to see them: the card fields a destination
/// is resolved from, plus whether this device is still willing to use that
/// card's endpoint. A value type on purpose — handing the canary the shell's
/// own `Contact` would hand it a store row; this carries only what a routing
/// decision reads. Mirrors Android `CoreRelayShadowContact`.
struct CoreRelayShadowContact {
    let userId: Data
    let relayUrl: String?
    let relayToken: String?
    let endpointUsable: Bool
}

/// The migration canary: on a few legacy passes a day, ask what the core engine
/// would have done with exactly what the legacy engine saw, and record where
/// the two disagree. Mirrors Android `RelayShadowAdapter`.
///
/// # The three hard rules, and how each is kept
///
/// **It performs no network I/O of its own.** Not by discipline — by the types
/// it is built from. What it can reach is a `RelayShadowReportSink` (one
/// closure), three closures returning plain values, and
/// `RelayShadowPassCapture`, whose every stored value is a byte array, a
/// number, a string or an enum. There is no `URLSession`, no `URL`, no
/// `RelayClient` and no driver anywhere in the capture, so there is nothing
/// here that *could* be asked to open a connection. The comparison itself is a
/// pure core function over those values, and `RelayShadowAdapterTests` pins
/// that shape by reflection so a field of a networking type cannot be added
/// quietly.
///
/// **It writes nothing to the production store.** It does not hold the store.
/// The one write it can reach is the sink's closure, which touches the bounded
/// diagnostics ring and takes a report that is counts and enums only. There is
/// no code path from a capture to a marker, a cursor, a receipt or a health
/// record, because there is no object here that has one.
///
/// **It cannot run against the core engine.** `relayShadowPermitted` is the
/// gate. Comparing the core planner against the core engine would agree every
/// time while looking exactly like evidence, which is worse than no canary.
///
/// # Its lifetime
///
/// It is deleted with the legacy engine. This is scaffolding that exists to
/// earn the evidence for that deletion, not an architecture.
final class RelayShadowAdapter {

    private let sink: RelayShadowReportSink
    private let passEngine: () -> RelayPassEngine
    private let shadowEnabled: () -> Bool
    private let loadSampler: () -> CoreRelayShadowSampler
    private let saveSampler: (CoreRelayShadowSampler) -> Void
    private let samplerLock = NSLock()
    private let log = Logger(subsystem: "com.cruisemesh", category: "RelayClient")

    init(
        sink: RelayShadowReportSink,
        passEngine: @escaping () -> RelayPassEngine,
        shadowEnabled: @escaping () -> Bool,
        loadSampler: @escaping () -> CoreRelayShadowSampler,
        saveSampler: @escaping (CoreRelayShadowSampler) -> Void
    ) {
        self.sink = sink
        self.passEngine = passEngine
        self.shadowEnabled = shadowEnabled
        self.loadSampler = loadSampler
        self.saveSampler = saveSampler
    }

    /// Begin capturing this pass, or return nil when the canary may not run at
    /// all.
    ///
    /// Cheap either way: this decides only whether capturing is *permitted*.
    /// Whether this pass is one of the sampled ones is decided the first time
    /// there is a row to compare, so a pass that turns out to have none spends
    /// nothing and leaves the day's budget for a pass that carries evidence.
    func beginPass(nowMs: Int64) -> RelayShadowPassCapture? {
        guard relayShadowPermitted(passEngine(), shadowEnabled: shadowEnabled()) else { return nil }
        return RelayShadowPassCapture { [weak self] in self?.armSample(nowMs: nowMs) ?? false }
    }

    private func armSample(nowMs: Int64) -> Bool {
        samplerLock.lock()
        defer { samplerLock.unlock() }
        let decision = coreRelayShadowSample(state: loadSampler(), nowMs: nowMs)
        saveSampler(decision.next)
        return decision.sample
    }

    /// Compare what was captured and record what was found.
    ///
    /// Called after the legacy pass has finished its uploads, so nothing this
    /// returns can change what that pass did — which is the other half of "no
    /// second writer": even a comparison that found a disagreement has no way
    /// to act on it.
    func finishPass(
        capture: RelayShadowPassCapture?,
        own: RelayConfig?,
        contacts: [CoreRelayShadowContact],
        nowMs: Int64
    ) {
        // `armed`, not `sampled`: asking whether this pass is a sampled one is
        // what *spends* a sample, and a pass that reached here without a row to
        // compare must not spend one on a comparison of nothing.
        guard let capture, capture.armed() else { return }
        // Everything from here is inside one guard, the sink call included.
        // `finishPass` is called from a `defer`/`finally`, so a throw here
        // would replace whatever error was unwinding the pass — a family rate
        // limit would surface as a plain failure, the health pill would say the
        // wrong thing, and the retry window would go unlogged. A canary must
        // never be the reason a pass reports a failure. Swift closures here do
        // not throw, but the same discipline keeps the sink's own failures from
        // escaping.
        let report = coreRelayShadowCompare(
            capture: CoreRelayShadowCapture(
                own: own.map { CoreRelayEndpointConfig(url: $0.relayUrl, token: $0.relayToken) },
                contacts: contacts.map {
                    CoreRelayContactConfig(
                        userId: $0.userId,
                        relayUrl: $0.relayUrl,
                        relayToken: $0.relayToken,
                        endpointUsable: $0.endpointUsable
                    )
                },
                steps: capture.steps(),
                skippedRecipients: capture.skippedRecipients(),
                rowsUnshadowed: capture.rowsUnshadowed()
            )
        )
        sink.note(report, nowMs)
        if !report.mismatches.isEmpty {
            log.warning(
                "Relay shadow found \(report.mismatches.count, privacy: .public) kind(s) of divergence over \(report.stepsCompared, privacy: .public) row(s); see the protocol event ring"
            )
        }
    }
}

/// What one sampled legacy pass remembers about its receipt and authored
/// uploads. Mirrors Android `RelayShadowPassCapture`.
///
/// Every method here takes values and returns nothing. It is not given the
/// relay config it is posting to as a live object, not given the connection,
/// not given a callback — only what was observed, after it was observed.
final class RelayShadowPassCapture {

    /// Mirrors `RELAY_SHADOW_MAX_ROWS`, read through the binding so the number
    /// is core's rather than this file's.
    private static let maxRows = Int(coreRelayShadowMaxRows())
    /// Mirrors `RELAY_SHADOW_MAX_SKIPS`, for the same reason.
    private static let maxSkips = Int(coreRelayShadowMaxSkips())

    /// Consulted once, at the first row worth comparing, and answers whether
    /// this pass is a sampled one. A closure rather than a flag so the day's
    /// budget is spent on a pass that has evidence in it.
    private let armSample: () -> Bool

    private var recordedSteps: [CoreRelayShadowStep] = []
    private var skipped: [Data] = []
    private var unshadowed = 0
    private var dropped = 0
    private var sampledDecision: Bool?

    /// The failed row each mailbox is still waiting to learn the answer for:
    /// did this pass go on to offer that mailbox the next row of the same lane?
    /// Keyed by lane and endpoint, because two mailboxes' rows interleave in one
    /// lane and the question is per mailbox.
    private var awaitingContinuation: [String: Int] = [:]

    init(armSample: @escaping () -> Bool) {
        self.armSample = armSample
    }

    /// A row the legacy engine posted and the relay accepted.
    func noteSucceeded(
        lane: CoreRelayShadowLane,
        msgId: Data,
        hopTtl: UInt8,
        recipientHint: Data,
        recipientUserId: Data,
        sealedLen: Int,
        expiryMs: Int64,
        endpoint: RelayConfig
    ) {
        record(
            lane: lane, msgId: msgId, hopTtl: hopTtl, recipientHint: recipientHint,
            recipientUserId: recipientUserId, sealedLen: sealedLen, expiryMs: expiryMs,
            endpoint: endpoint, status: 200, relayCode: nil, transportError: nil,
            markedPosted: true
        )
    }

    /// A row the legacy engine posted and the relay or the link refused.
    func noteFailed(
        lane: CoreRelayShadowLane,
        msgId: Data,
        hopTtl: UInt8,
        recipientHint: Data,
        recipientUserId: Data,
        sealedLen: Int,
        expiryMs: Int64,
        endpoint: RelayConfig,
        error: Error
    ) {
        // The relay's own answer when it gave one, and a classified transport
        // failure only when it did not.
        let http = error as? RelayHTTPError
        record(
            lane: lane, msgId: msgId, hopTtl: hopTtl, recipientHint: recipientHint,
            recipientUserId: recipientUserId, sealedLen: sealedLen, expiryMs: expiryMs,
            endpoint: endpoint,
            status: http?.statusCode ?? 0,
            relayCode: http?.relayCode,
            transportError: http == nil ? relayClassifyTransportError(error) : nil,
            markedPosted: false
        )
    }

    /// A row the legacy engine declined to post at all, having resolved no
    /// mailbox for it.
    func noteDeclined(
        lane: CoreRelayShadowLane,
        msgId: Data,
        hopTtl: UInt8,
        recipientHint: Data,
        recipientUserId: Data,
        sealedLen: Int,
        expiryMs: Int64
    ) {
        record(
            lane: lane, msgId: msgId, hopTtl: hopTtl, recipientHint: recipientHint,
            recipientUserId: recipientUserId, sealedLen: sealedLen, expiryMs: expiryMs,
            endpoint: nil, status: 0, relayCode: nil, transportError: nil,
            markedPosted: false
        )
    }

    private func record(
        lane: CoreRelayShadowLane,
        msgId: Data,
        hopTtl: UInt8,
        recipientHint: Data,
        recipientUserId: Data,
        sealedLen: Int,
        expiryMs: Int64,
        endpoint: RelayConfig?,
        status: Int,
        relayCode: String?,
        transportError: CoreRelayTransportError?,
        markedPosted: Bool
    ) {
        guard sampled() else { return }
        if recordedSteps.count >= Self.maxRows {
            // Counted rather than silently forgotten: a report that compared
            // sixteen of forty rows must not read as a report about forty.
            dropped += 1
            unshadowed += 1
            return
        }
        let succeeded = markedPosted && (200..<300).contains(status)
        let key = endpoint.map { "\(lane)|\($0.relayUrl)|\($0.relayToken)" }
        if let key {
            // This row is being offered to a mailbox some earlier row of this
            // lane failed against, which is the whole of what "the lane
            // continued" means. Observed rather than predicted: deriving it
            // from the error type says "true" even when there was no next row
            // for that mailbox to offer, which is most failures.
            if let waiting = awaitingContinuation.removeValue(forKey: key) {
                recordedSteps[waiting].legacyContinuedLane = true
            }
        }
        recordedSteps.append(
            CoreRelayShadowStep(
                lane: lane,
                msgId: msgId,
                hopTtl: hopTtl,
                recipientHint: recipientHint,
                recipientUserId: recipientUserId,
                sealedLen: UInt64(max(0, sealedLen)),
                expiryMs: expiryMs,
                legacyEndpoint: endpoint.map { CoreRelayEndpointConfig(url: $0.relayUrl, token: $0.relayToken) },
                status: UInt16(clamping: status),
                relayCode: relayCode,
                transportError: transportError,
                legacyMarkedPosted: markedPosted,
                // Starts equal to whether this row itself succeeded and is
                // corrected above if a later row is actually offered to the same
                // mailbox. A pass that ends here answered "no".
                legacyContinuedLane: succeeded
            )
        )
        if !succeeded, let key { awaitingContinuation[key] = recordedSteps.count - 1 }
    }

    /// Recipients the legacy engine excluded from its queue query before
    /// selecting anything.
    ///
    /// Buffered whether or not this pass turns out to be sampled, and
    /// deliberately not a reason to spend a sample: a device with one retired
    /// friend card reports the same skip on every pass it ever runs, so arming
    /// on a skip list would arm on every pass and put the budget back where it
    /// started.
    func noteSkippedRecipients(_ recipients: [Data]) {
        for recipient in recipients {
            if skipped.count >= Self.maxSkips { return }
            skipped.append(recipient)
        }
    }

    /// Rows this capture deliberately cannot speak for: group fan-out rows,
    /// which core's upload lanes do not decompose, and carried rows, which a
    /// later package owns.
    ///
    /// Counted whether or not the sample has been armed yet, and never a reason
    /// to arm it. Both halves matter: a mule pass that carries forty rows and
    /// compares none is not evidence worth a sample, and a group row that went
    /// out before the first authored row would otherwise be missing from a
    /// report the authored row does earn.
    func noteUnshadowed(_ rows: Int) {
        if rows > 0 { unshadowed += rows }
    }

    /// Whether this pass is one of the sampled ones, deciding it on first ask.
    ///
    /// A capture belongs to the one pass that created it and a pass runs on one
    /// thread; the sampler state behind `armSample` is the shared thing, and
    /// that is guarded where it lives.
    func sampled() -> Bool {
        if let sampledDecision { return sampledDecision }
        let decision = armSample()
        sampledDecision = decision
        return decision
    }

    /// Whether a sample was already spent on this pass. Asks nothing and decides
    /// nothing, which is what makes it safe to call at the end of a pass that
    /// may have had no rows at all.
    func armed() -> Bool { sampledDecision == true }

    func steps() -> [CoreRelayShadowStep] { recordedSteps }
    func skippedRecipients() -> [Data] { skipped }
    func rowsUnshadowed() -> UInt32 { UInt32(clamping: unshadowed) }
    func rowsDropped() -> Int { dropped }
}
